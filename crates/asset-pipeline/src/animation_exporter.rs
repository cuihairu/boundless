//! BW-13-05-02: the Max animation exporter's portable core
//! (`tools/animationexporter/`).
//!
//! The reference exporter is a 3ds Max plugin (`mfxexp` walking the scene
//! through the `max.h` SDK); the `max2009`..`max2013` directories only
//! select which SDK to compile against. The live write path is
//! `MFXExport::DoExport` → `MFXNode::exportAnimation` →
//! `CueTrack::writeFile`: one `.animation` binary holding an f32 total
//! time, the animation name twice, an i32 channel count, then per
//! included node an interpolated channel (type 1, or type 4 with the
//! three compression errors harvested from the existing file) and
//! optionally a cue channel (type 6).
//!
//! Ported faces:
//! - [`ExportSettings`] with its five serialized keys and the
//!   `animationexporter.cfg` / `<stem>.animationsettings` /
//!   maxscript-override read chain (`expsets.cpp` defaults,
//!   `expsetsio.cpp`, the `BWAnimationSetting` primitive);
//! - the [`SceneNode`] scene model and `preProcess` classification
//!   (`mfxexp.cpp:540-610`): the node filter, the forced class
//!   exclusions and the mesh/envelope split;
//! - the [`MfxTree`] node arena: `getTransform`/`getRelativeTransform`,
//!   the reference-nodes re-parent, the descending root-child sort,
//!   `mungeIncluded`, the duplicate-name validation and
//!   `exportAnimation` itself;
//! - [`rearrange_matrix`] (`utility.cpp`) with the translation row
//!   behaviour pinned by shipped content (see its doc), the
//!   `BlendTransform::init` decompose (`lib/math/blend_transform.cpp`)
//!   and the [`CueTrack`] note-track channel.
//!
//! Host seams and port deviations, each behaviour-preserving for the
//! `.animation` bytes:
//! - `displayDialog`, `MessageBox` and `LogMsg` are the plugin GUI
//!   faces; the illegal-node-name warning is collected in
//!   [`MfxExporter::warnings`] and the res-path abort becomes an
//!   [`ExportError::NotInResPath`] carrying the verbatim message;
//! - `exportMeshes`/`exportEnvelopes` write the companion visual output
//!   through `VisualMesh`/`VisualEnvelope` (Max object conversion and
//!   the NVMeshMender seam — [`crate::visual_exporter`]); the port
//!   keeps the classification (`mesh_nodes`/`envelope_nodes`) and
//!   leaves the companion write to the host;
//! - `GetNodeTM(t)` world transforms arrive precomputed on
//!   [`SceneNode::world`], one matrix per exported frame;
//! - `Inverse()` of a singular relative transform produces uninitialised
//!   memory in the reference; the port substitutes the zero matrix;
//! - the `StringHashMap` behind `nodeParents_` has an unspecified
//!   iteration order — the port applies the pairs in first-insertion
//!   order with last-wins parents;
//! - reads off an existing `.animation` harvest the compression errors
//!   into locals the reference leaves uninitialised on a short read;
//!   the port substitutes zeros.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use foundation::math::{Matrix, Quaternion, Vector3};
use resources::datasection::DataSection;

// ===========================================================================
// Scene input model
// ===========================================================================

/// The object classification `preProcess` reads off `EvalWorldState`
/// (`mfxexp.cpp:559-577`); every non-[`NodeKind::Object`] kind is forced
/// out of the animation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NodeKind {
    /// A plain geometric or helper object.
    #[default]
    Object,
    /// `Class_ID(TARGET_CLASS_ID, 0)` look-at targets.
    Target,
    /// `Class_ID(0x74f93b07, 0x1eb34300)` particle-view helpers.
    ParticleView,
    /// `CAMERA_CLASS_ID`.
    Camera,
    /// `LIGHT_CLASS_ID`.
    Light,
    /// `SHAPE_CLASS_ID`.
    Shape,
}

/// One Max scene node (`INode`), as far as the animation export observes
/// it.
#[derive(Debug, Clone, Default)]
pub struct SceneNode {
    /// `GetName()`.
    pub name: String,
    /// The `EvalWorldState(staticFrame)` classification.
    pub kind: NodeKind,
    /// `IsHidden()`.
    pub hidden: bool,
    /// `Selected()`.
    pub selected: bool,
    /// `findPhysiqueModifier(node) || findSkinMod(node)`.
    pub skinned: bool,
    /// Note-track keys `(ticks, text)` (`DefNoteTrack::keys`), harvested
    /// when `exportCueTrack` is set.
    pub note_keys: Vec<(i32, String)>,
    /// `GetNodeTM(t)` per exported frame, indexed `frame - first_frame`
    /// (frames run `first..=last`).
    pub world: Vec<Matrix>,
    /// The scene children, in Max order.
    pub children: Vec<SceneNode>,
}

// ===========================================================================
// Export settings
// ===========================================================================

/// `ExportSettings::NodeFilter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeFilter {
    /// `ALL`.
    All,
    /// `SELECTED`.
    Selected,
    /// `VISIBLE`.
    Visible,
}

/// `CFGFilename` (`mfxexp.hpp`).
pub const CFG_FILENAME: &str = "animationexporter.cfg";

/// The animation flavour of `ExportSettings` (`expsets.cpp`).
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSettings {
    /// `allowScale_`.
    pub allow_scale: bool,
    /// `exportNodeAnimation_`.
    pub export_node_animation: bool,
    /// `exportCueTrack_`.
    pub export_cue_track: bool,
    /// `useLegacyOrientation_`.
    pub use_legacy_orientation: bool,
    /// `nodeFilter_`.
    pub node_filter: NodeFilter,
    /// `staticFrame_`.
    pub static_frame: i32,
    /// `frameFirst_`.
    pub frame_first: i32,
    /// `frameLast_`.
    pub frame_last: i32,
    /// `referenceNodesFile_`.
    pub reference_nodes_file: String,
}

impl Default for ExportSettings {
    /// The reference constructor (`expsets.cpp:229-238`).
    fn default() -> Self {
        Self {
            static_frame: 0,
            frame_first: 0,
            frame_last: 100,
            node_filter: NodeFilter::Visible,
            allow_scale: false,
            export_node_animation: true,
            export_cue_track: false,
            use_legacy_orientation: false,
            reference_nodes_file: String::new(),
        }
    }
}

impl ExportSettings {
    /// `ExportSettings::setFrameRange`.
    pub fn set_frame_range(&mut self, first: i32, last: i32) {
        self.frame_first = first;
        self.frame_last = last;
    }

    /// `ExportSettings::readSettings(DataSectionPtr)` (`expsetsio.cpp`):
    /// five keys, each keeping the current value as its default.
    pub fn read_settings(&mut self, section: Option<&DataSection>) -> bool {
        let Some(section) = section else {
            return false;
        };
        self.allow_scale = section.read_bool("allowScale", self.allow_scale);
        self.export_node_animation =
            section.read_bool("exportNodeAnimation", self.export_node_animation);
        self.reference_nodes_file =
            section.read_string("referenceNodesFile", &self.reference_nodes_file);
        self.export_cue_track = section.read_bool("exportCueTrack", self.export_cue_track);
        self.use_legacy_orientation =
            section.read_bool("useLegacyOrientation", self.use_legacy_orientation);
        true
    }

    /// `ExportSettings::writeSettings(DataSectionPtr)`.
    pub fn write_settings(&self, section: Option<&mut DataSection>) -> bool {
        let Some(section) = section else {
            return false;
        };
        section.write_bool("allowScale", self.allow_scale);
        section.write_bool("exportNodeAnimation", self.export_node_animation);
        section.write_string("referenceNodesFile", &self.reference_nodes_file);
        section.write_bool("exportCueTrack", self.export_cue_track);
        section.write_bool("useLegacyOrientation", self.use_legacy_orientation);
        true
    }
}

// ===========================================================================
// Cue track
// ===========================================================================

/// The note-track cue channel (`cuetrack.cpp`). The reference keeps a
/// singleton alive across plugin invocations (cleared at export time when
/// the cue track is wanted); the port owns one per exporter.
#[derive(Debug, Clone, Default)]
pub struct CueTrack {
    /// Adjusted ticks → cue texts in add order (the `BW::map<TimeValue,
    /// BW::list<BW::string>>`).
    cues: BTreeMap<i32, Vec<String>>,
}

impl CueTrack {
    /// `CueTrack::clear`.
    pub fn clear(&mut self) {
        self.cues.clear();
    }

    /// `CueTrack::addCue`: the cue frame must sit inside the exported
    /// range, the time shifts back by the first frame, and only
    /// `cue:`/`sound:` notes pass; duplicates at the same time are
    /// dropped.
    pub fn add_cue(
        &mut self,
        mut time: i32,
        note: &str,
        tpf: i32,
        first_frame: i32,
        last_frame: i32,
    ) {
        // Check if the cue is among the frames being exported (C integer
        // division, like the reference's `time / GetTicksPerFrame()`).
        let cue_frame = time / tpf;
        if cue_frame < first_frame || cue_frame > last_frame {
            return;
        }

        // Adjust the time based on the first frame.
        time -= first_frame * tpf;

        // string for the note track is expected to be of the form
        // cue:name or sound:name
        let Some(split) = note.find(':') else {
            return;
        };

        let cue = match &note[..split] {
            "sound" => format!("s{}", &note[split + 1..]),
            "cue" => format!("c{}", &note[split + 1..]),
            // only cue: and sound: are supported at the moment
            _ => return,
        };

        let list = self.cues.entry(time).or_default();
        if list.contains(&cue) {
            return; // already have the cue for that time
        }
        list.push(cue);
    }

    /// `CueTrack::hasCues`.
    pub fn has_cues(&self) -> bool {
        self.cues.values().any(|list| !list.is_empty())
    }

    /// `CueTrack::writeFile`: with no cues nothing at all is written —
    /// not even the type marker.
    pub fn write_file(&self, out: &mut Vec<u8>, tpf: i32) {
        let num: usize = self.cues.values().map(|list| list.len()).sum();

        // if no cues, don't even bother writing the cue track
        if num == 0 {
            return;
        }

        // write channel identifier
        w_i32(out, 6);

        // write number of cues
        w_i32(out, num as i32);

        // write cue data
        for (time, list) in &self.cues {
            for cue in list {
                w_f32(out, *time as f32 / tpf as f32);
                w_str(out, cue);

                // no additional args
                w_i32(out, 0);
            }
        }
    }

    /// The cue map, sorted by adjusted ticks.
    pub fn cues(&self) -> &BTreeMap<i32, Vec<String>> {
        &self.cues
    }
}

// ===========================================================================
// Binary writers (`BinaryFile` output)
// ===========================================================================

fn w_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn w_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// `operator<<(BinaryFile&, const BW::string&)`: the int length, then the
/// bytes.
fn w_str(out: &mut Vec<u8>, s: &str) {
    w_i32(out, s.len() as i32);
    out.extend_from_slice(s.as_bytes());
}

/// `writeSequence` over `pair<float, Vector3>`: the u32 count, then count
/// × raw POD (`f32` time + 3 × `f32`).
fn w_vec3_seq(out: &mut Vec<u8>, keys: &[(f32, Vector3)]) {
    w_i32(out, keys.len() as i32);
    for (t, v) in keys {
        w_f32(out, *t);
        w_f32(out, v.x);
        w_f32(out, v.y);
        w_f32(out, v.z);
    }
}

/// `writeSequence` over `pair<float, Quaternion>`: raw POD, 4 × `f32` per
/// element.
fn w_quat_seq(out: &mut Vec<u8>, keys: &[(f32, Quaternion)]) {
    w_i32(out, keys.len() as i32);
    for (t, q) in keys {
        w_f32(out, *t);
        w_f32(out, q.x);
        w_f32(out, q.y);
        w_f32(out, q.z);
        w_f32(out, q.w);
    }
}

/// `writeSequence` over `int`.
fn w_i32_seq(out: &mut Vec<u8>, values: &[i32]) {
    w_i32(out, values.len() as i32);
    for v in values {
        w_i32(out, *v);
    }
}

// ===========================================================================
// utility.cpp + blend_transform.cpp math
// ===========================================================================

/// `utility.cpp rearrangeMatrix`: the basis swap that maps Max's space
/// into BigWorld's. The reference builds `mm` with three `SetColumn`
/// calls (rows 0..2 only) and one row swap, so the upper 3×3 is `m` with
/// axis 1 and axis 2 exchanged (`S·m·S`). The translation row passes
/// through untouched — pinned by shipped content: `crab.visual`'s
/// `CrabHub` row 3 `(0.001331, 0.100967, 0.000757)` equals
/// `crab_walkforward.animation`'s `CrabHub` t=0 position key, which is
/// the rearranged row 3 times the unit scale. The legacy orientation
/// negates rows 0/2 and then columns 0/2 (rows 0..2), so every 3×3
/// element with exactly one index in `{0, 2}` flips sign; row 3 is never
/// touched there either.
fn rearrange_matrix(m: &Matrix, legacy: bool) -> Matrix {
    let mut out = Matrix::zero();

    // Column swap over rows 0..2: col0 ← col0, col1 ← col2, col2 ← col1.
    for (dst, src) in [(0usize, 0usize), (1, 2), (2, 1)] {
        for row in 0..3 {
            out.m[row][dst] = m.m[row][src];
        }
    }

    // Row swap 1 <-> 2 over rows 0..2.
    for col in 0..3 {
        let row1 = out.m[1][col];
        let row2 = out.m[2][col];
        out.m[1][col] = row2;
        out.m[2][col] = row1;
    }

    // The translation row survives the whole rearrange.
    out.m[3] = m.m[3];

    if legacy {
        // The sign pattern covers the upper 3x3 only; the translation
        // column and row are never touched.
        for (row, row_vals) in out.m[..3].iter_mut().enumerate() {
            for (col, value) in row_vals[..3].iter_mut().enumerate() {
                if (row == 0 || row == 2) ^ (col == 0 || col == 2) {
                    *value = -*value;
                }
            }
        }
    }

    out
}

/// `utility.cpp normaliseMatrix`: rows 0..2 renormalised, row 3 passed
/// through. A zero row stays zero (`Normalize` of a zero vector is the
/// reference's NaN path — unreachable for sane world transforms).
fn normalise_matrix(m: &Matrix) -> Matrix {
    let mut out = Matrix::zero();
    for row in 0..3 {
        let mut v = Vector3::new(m.m[row][0], m.m[row][1], m.m[row][2]);
        let length = v.length();
        if length > 0.0 {
            v.scale(1.0 / length);
        }
        out.m[row][0] = v.x;
        out.m[row][1] = v.y;
        out.m[row][2] = v.z;
    }
    out.m[3] = m.m[3];
    out
}

/// `utility.cpp trailingLeadingWhitespaces`.
fn trailing_leading_whitespaces(s: &str) -> bool {
    !s.is_empty() && (s.starts_with(' ') || s.ends_with(' '))
}

/// `blend_transform.cpp BlendTransform::init`: the decompose every
/// channel key goes through. The row lengths are the scale, rows 0..2 are
/// renormalised in place, a negative cross/dot flips row 2 and the z
/// scale, the translation row passes through, and the rotation is the
/// quaternion of the normalised matrix — the host's
/// `XPQuaternionRotationMatrix` is `D3DXQuaternionRotationMatrix`, which
/// produces the same values as `Quaternion::from_matrix` (only the
/// exactly-equal diagonal tie-break can differ).
fn blend_decompose(m: &Matrix) -> (Quaternion, Vector3, Vector3) {
    let mut m = *m;

    fn row_vec(m: &Matrix, row: usize) -> Vector3 {
        Vector3::new(m.m[row][0], m.m[row][1], m.m[row][2])
    }
    let mut scale = Vector3::new(
        row_vec(&m, 0).length(),
        row_vec(&m, 1).length(),
        row_vec(&m, 2).length(),
    );

    for (row, s) in [(0usize, scale.x), (1, scale.y), (2, scale.z)] {
        for value in &mut m.m[row] {
            *value *= 1.0 / s;
        }
    }

    let cross = row_vec(&m, 0).cross_product(&row_vec(&m, 0), &row_vec(&m, 1));
    if cross.dot_product(&row_vec(&m, 2)) < 0.0 {
        for value in &mut m.m[2] {
            *value *= -1.0;
        }
        scale.z *= -1.0;
    }

    let translation = row_vec(&m, 3);
    let rotation = Quaternion::from_matrix(&m);
    (rotation, scale, translation)
}

/// `BWResource::removeExtension`: everything before the last dot; without
/// a dot the input passes through.
fn remove_extension(path: &str) -> &str {
    match path.rfind('.') {
        Some(i) => &path[..i],
        None => path,
    }
}

// ===========================================================================
// Node tree
// ===========================================================================

/// The flattened scene payload one [`MfxNode`] observes (the world
/// transforms arrive per exported frame).
#[derive(Debug, Clone, Default)]
pub(crate) struct FlatScene {
    pub name: String,
    pub world: Vec<Matrix>,
}

pub(crate) struct MfxNode {
    /// The stored identifier (used once the max node is detached).
    identifier: String,
    /// The `INode` attachment, as a flat-scene index (`setMaxNode(NULL)`
    /// clears it without touching the identifier).
    scene: Option<usize>,
    parent: Option<usize>,
    children: Vec<usize>,
    include: bool,
    /// The stored transform (identity; only non-scene nodes read it).
    transform: Matrix,
}

/// The `MFXNode` arena behind one export (`mfxnode.cpp`).
pub(crate) struct MfxTree {
    pub nodes: Vec<MfxNode>,
    pub root: usize,
    pub flat: Vec<FlatScene>,
    /// `meshNodes_` — flat-scene indices in pre-order (the Max-host
    /// companion visual export consumes them).
    pub mesh_nodes: Vec<usize>,
    /// `envelopeNodes_` — skinned nodes, which lose their include flag.
    pub envelope_nodes: Vec<usize>,
}

impl MfxTree {
    /// `MFXNode::getIdentifier`: the live scene name while a max node is
    /// attached, the stored identifier otherwise.
    pub fn identifier(&self, n: usize) -> &str {
        match self.nodes[n].scene {
            Some(flat) => &self.flat[flat].name,
            None => &self.nodes[n].identifier,
        }
    }

    /// `MFXNode::find(identifier)`: DFS pre-order, first match.
    pub fn find(&self, identifier: &str) -> Option<usize> {
        if self.identifier(self.root) == identifier {
            return Some(self.root);
        }
        self.find_in(self.root, identifier)
    }

    fn find_in(&self, n: usize, identifier: &str) -> Option<usize> {
        for &child in &self.nodes[n].children {
            if self.identifier(child) == identifier {
                return Some(child);
            }
            if let Some(found) = self.find_in(child, identifier) {
                return Some(found);
            }
        }
        None
    }

    /// `MFXNode::getTransform(t, normalise)`: the world transform of the
    /// max node, else the parent chain times the stored transform, else
    /// the stored transform.
    pub fn get_transform(&self, n: usize, frame_index: usize, normalise: bool) -> Matrix {
        let node = &self.nodes[n];
        if let Some(flat) = node.scene {
            let world = self.flat[flat]
                .world
                .get(frame_index)
                .copied()
                .unwrap_or(Matrix::identity());
            return if normalise {
                normalise_matrix(&world)
            } else {
                world
            };
        }

        if let Some(parent) = node.parent {
            let parent_tm = self.get_transform(parent, frame_index, false);
            let mut out = Matrix::zero();
            out.multiply(&parent_tm, &node.transform);
            return if normalise {
                normalise_matrix(&out)
            } else {
                out
            };
        }

        if normalise {
            normalise_matrix(&node.transform)
        } else {
            node.transform
        }
    }

    /// `MFXNode::getRelativeTransform(t, normalise, idealParent)`: the
    /// world transform times the inverse of the ideal parent's; a node
    /// without a max node answers its stored transform.
    pub fn get_relative_transform(
        &self,
        n: usize,
        frame_index: usize,
        normalise: bool,
        ideal_parent: Option<usize>,
    ) -> Matrix {
        if self.nodes[n].scene.is_some() {
            // Do we want to override the parent?
            let mut ideal = ideal_parent;
            if self.nodes[n].parent.is_some() && ideal.is_none() {
                ideal = self.nodes[n].parent;
            }

            // If there is a parent get the relative transform, from the
            // parent, else return this node's transform.
            if let Some(ideal) = ideal {
                let mut inv = self.get_transform(ideal, frame_index, normalise);
                // `Inverse()` of a singular matrix is garbage memory in the
                // reference; the port substitutes the zero matrix.
                if !inv.invert() {
                    inv = Matrix::zero();
                }
                let mut out = Matrix::zero();
                out.multiply(&self.get_transform(n, frame_index, normalise), &inv);
                return out;
            }
            return self.get_transform(n, frame_index, normalise);
        }

        if normalise {
            normalise_matrix(&self.nodes[n].transform)
        } else {
            self.nodes[n].transform
        }
    }

    /// `MFXNode::nIncludedNodes`.
    pub fn n_included_nodes(&self) -> usize {
        self.n_included_from(self.root)
    }

    fn n_included_from(&self, n: usize) -> usize {
        let mut included = usize::from(self.nodes[n].include);
        for &child in &self.nodes[n].children {
            included += self.n_included_from(child);
        }
        included
    }

    /// `MFXNode::includeAncestors`.
    fn include_ancestors(&mut self, mut n: usize) {
        while let Some(parent) = self.nodes[n].parent {
            self.nodes[parent].include = true;
            n = parent;
        }
    }

    /// `mungeIncluded`: every included node re-includes its ancestor chain.
    pub fn munge_included(&mut self) {
        let order = self.pre_order(self.root);
        for n in order {
            if self.nodes[n].include {
                self.include_ancestors(n);
            }
        }
    }

    fn pre_order(&self, n: usize) -> Vec<usize> {
        let mut out = vec![n];
        for &child in &self.nodes[n].children {
            out.extend(self.pre_order(child));
        }
        out
    }

    fn detach(&mut self, n: usize) {
        if let Some(parent) = self.nodes[n].parent {
            self.nodes[parent].children.retain(|&c| c != n);
        }
        self.nodes[n].parent = None;
    }

    fn attach(&mut self, parent: usize, n: usize) {
        self.nodes[parent].children.push(n);
        self.nodes[n].parent = Some(parent);
    }

    /// One `nodeParents_` entry applied (`mfxexp.cpp:339-366`): when `p`
    /// is not `c`'s parent, the subtree of `p` that hangs off `c` moves
    /// back onto `c`'s parent first, then `c` moves under `p`. The port
    /// keeps the `c->getParent()->addChild(parent)` arm defined when `c`
    /// is the root (the reference would dereference NULL there).
    pub fn reparent(&mut self, c: usize, p: usize) {
        if self.nodes[c].parent == Some(p) {
            return;
        }

        // Walk from `p` up to the node whose parent is `c`.
        let mut split = None;
        let mut walk = Some(p);
        while let Some(w) = walk {
            if self.nodes[w].parent == Some(c) {
                split = Some(w);
                break;
            }
            walk = self.nodes[w].parent;
        }
        if let Some(split) = split {
            self.detach(split);
            if let Some(cp) = self.nodes[c].parent {
                self.attach(cp, split);
            }
        }

        let op = self.nodes[c].parent;
        if op.is_some() {
            self.detach(c);
        }
        self.attach(p, c);
    }

    /// The DoExport root reorder: detach all children, stable-sort them
    /// descending by child count (`std::list::sort` is stable), re-attach.
    pub fn sort_root_children(&mut self) {
        let mut children = std::mem::take(&mut self.nodes[self.root].children);
        for &c in &children {
            self.nodes[c].parent = None;
        }
        children.sort_by(|&a, &b| {
            self.nodes[b]
                .children
                .len()
                .cmp(&self.nodes[a].children.len())
        });
        for c in children {
            self.attach(self.root, c);
        }
    }

    /// `checkDuplicateNodeNames` + `validateAnimationIDs`' message: the
    /// sorted duplicate identifiers of the included nodes.
    pub fn duplicate_names(&self) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut duplicates = BTreeSet::new();
        self.check_duplicate_names(self.root, &mut seen, &mut duplicates);
        duplicates
    }

    fn check_duplicate_names(
        &self,
        n: usize,
        seen: &mut BTreeSet<String>,
        duplicates: &mut BTreeSet<String>,
    ) {
        if self.nodes[n].include {
            let id = self.identifier(n).to_string();
            if !seen.insert(id.clone()) {
                duplicates.insert(id);
            }
        }
        for &child in &self.nodes[n].children {
            self.check_duplicate_names(child, seen, duplicates);
        }
    }

    /// `MFXNode::exportAnimation`: the key sequences plus the channel
    /// header for this node, then the children.
    #[allow(clippy::too_many_arguments)]
    pub fn export_animation(
        &self,
        n: usize,
        out: &mut Vec<u8>,
        ci: &CompressionInfo,
        settings: &ExportSettings,
        unit_scale: f32,
        warnings: &mut Vec<String>,
        ideal_parent: Option<usize>,
    ) {
        let mut next_parent = ideal_parent;

        if self.nodes[n].include {
            let identifier = self.identifier(n).to_string();
            if trailing_leading_whitespaces(&identifier) {
                warnings.push(format!(
                    "Node \"{}\" has an illegal name. Remove any spaces at the beginning and end of the name.",
                    identifier
                ));
            }

            next_parent = Some(n);
            let first_frame = settings.frame_first;
            let normalise = !settings.allow_scale;

            let mut bound_table = Vec::new();
            let mut scales: Vec<(f32, Vector3)> = Vec::new();
            let mut positions: Vec<(f32, Vector3)> = Vec::new();
            let mut rotations: Vec<(f32, Quaternion)> = Vec::new();

            let mut time = 0.0f32;
            for frame in settings.frame_first..=settings.frame_last {
                let rel = self.get_relative_transform(
                    n,
                    (frame - first_frame) as usize,
                    normalise,
                    ideal_parent,
                );
                let rearranged = rearrange_matrix(&rel, settings.use_legacy_orientation);
                // Rows go over as 3-float `Vector3`s, so the fourth column
                // keeps the identity's values.
                let mut m = Matrix::identity();
                for (dst, src) in [(0usize, 0usize), (1, 1), (2, 2), (3, 3)] {
                    for col in 0..3 {
                        m.m[dst][col] = rearranged.m[src][col];
                    }
                }
                let (mut rotation, scale, mut translation) = blend_decompose(&m);

                // Normalise the rotation quaternion.
                rotation.normalise();

                bound_table.push(frame - first_frame + 1);
                scales.push((time, scale));
                translation.scale(unit_scale);
                positions.push((time, translation));
                rotations.push((time, rotation));
                time += 1.0;
            }

            if ci.specify_amounts {
                w_i32(out, 4);
                w_str(out, &identifier);
                w_f32(out, ci.scale_compression_error);
                w_f32(out, ci.position_compression_error);
                w_f32(out, ci.rotation_compression_error);
            } else {
                w_i32(out, 1);
                w_str(out, &identifier);
            }

            w_vec3_seq(out, &scales);
            w_vec3_seq(out, &positions);
            w_quat_seq(out, &rotations);
            w_i32_seq(out, &bound_table);
            w_i32_seq(out, &bound_table);
            w_i32_seq(out, &bound_table);
        }

        for &child in &self.nodes[n].children {
            self.export_animation(child, out, ci, settings, unit_scale, warnings, next_parent);
        }
    }
}

// ===========================================================================
// Compression info harvest
// ===========================================================================

/// `CompressionInfo` (`mfxnode.hpp`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompressionInfo {
    /// `specifyAmounts_`.
    pub specify_amounts: bool,
    /// `scaleCompressionError_`.
    pub scale_compression_error: f32,
    /// `positionCompressionError_`.
    pub position_compression_error: f32,
    /// `rotationCompressionError_`.
    pub rotation_compression_error: f32,
}

impl Default for CompressionInfo {
    fn default() -> Self {
        Self {
            specify_amounts: false,
            scale_compression_error: 0.0,
            position_compression_error: 0.0,
            rotation_compression_error: 0.0,
        }
    }
}

/// A minimal `BinaryFile >>` cursor over the existing file (short reads
/// hand out zeros — the reference leaves its locals uninitialised there).
struct Harvest<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Harvest<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn f32(&mut self) -> f32 {
        let bytes = self.take(4);
        match bytes {
            Some(b) => f32::from_le_bytes(b.try_into().unwrap()),
            None => 0.0,
        }
    }

    fn i32(&mut self) -> i32 {
        let bytes = self.take(4);
        match bytes {
            Some(b) => i32::from_le_bytes(b.try_into().unwrap()),
            None => 0,
        }
    }

    fn string(&mut self) -> String {
        let len = self.i32();
        if len < 0 {
            return String::new();
        }
        match self.take(len as usize) {
            Some(b) => String::from_utf8_lossy(b).into_owned(),
            None => String::new(),
        }
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.pos + len > self.data.len() {
            self.pos = self.data.len();
            return None;
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Some(slice)
    }
}

/// The `CompressionInfo` harvest off the existing `.animation`
/// (`DoExport`'s `_wfopen("rb")` block): the header, and from the first
/// channel the type — a type 4 channel donates the three errors.
fn harvest_compression_info(data: &[u8]) -> CompressionInfo {
    let mut ci = CompressionInfo::default();
    let mut r = Harvest::new(data);

    // Read in the header
    let _total = r.f32();
    let _name = r.string();
    let _name2 = r.string();
    let num_channels = r.i32();

    // Get the compression amounts from the first channel
    if num_channels > 0 {
        let channel_type = r.i32();
        // Provided it's the correct type
        if channel_type == 4 {
            let _s = r.string();
            ci.specify_amounts = true;
            ci.scale_compression_error = r.f32();
            ci.position_compression_error = r.f32();
            ci.rotation_compression_error = r.f32();
        }
    }

    ci
}

/// `MFXExport::readReferenceHierarchy`: the child → parent identifier
/// pairs of a visual node tree, in document order.
fn read_reference_hierarchy(section: &DataSection, out: &mut Vec<(String, String)>) {
    let id = section.read_string("identifier", "");
    for i in 0..section.count_children() {
        if let Some(node) = section.open_child(i)
            && node.section_name() == "node"
        {
            out.push((node.read_string("identifier", ""), id.clone()));
            read_reference_hierarchy(node, out);
        }
    }
}

// ===========================================================================
// Host seam
// ===========================================================================

/// The host faces `DoExport` reaches through the Max SDK, `BWResource` and
/// the C runtime.
pub trait AnimationIo {
    /// `BWResolver::dissolveFilename`: the resource-relative path, or the
    /// input unchanged when it is not under a res path.
    fn dissolve_filename(&self, filename: &str) -> String;

    /// `BWResource::getPath(i)` until `""` — the valid game paths listed
    /// by the res-path abort message.
    fn res_paths(&self) -> Vec<String>;

    /// `BWResource::openSection` (the reference-nodes visual file).
    fn open_section(&self, filename: &str) -> Option<DataSection>;

    /// `DataResource(filename, RESOURCE_TYPE_XML)` read: the root
    /// section, or `None` when the file does not exist.
    fn read_settings_xml(&self, filename: &str) -> Option<DataSection>;

    /// The `DataResource::save` write face.
    fn write_settings_xml(&mut self, filename: &str, root: &DataSection);

    /// `_wfopen(filename, "rb")`: the existing `.animation` bytes.
    fn read_animation(&self, filename: &str) -> Option<Vec<u8>>;

    /// `_wfopen(filename, "wb")` plus the write: false when the host
    /// refuses (the reference then skips the settings rewrite too).
    fn write_animation(&mut self, filename: &str, bytes: &[u8]) -> bool;

    /// `::DeleteFile(settingsFilename)`.
    fn delete_file(&mut self, filename: &str);

    /// `getCfgFilename`: the Max plugin config directory plus
    /// `CFGFilename`; the directory is the host's, so the default is the
    /// bare file name.
    fn cfg_filename(&self) -> String {
        CFG_FILENAME.to_string()
    }

    /// `GetTicksPerFrame()`: the Max default.
    fn ticks_per_frame(&self) -> i32 {
        160
    }

    /// `GetMasterScale(UNITS_CENTIMETERS)/100`: with the host's default
    /// system unit of centimetres the factor is 1, so `0.01`.
    fn unit_scale(&self) -> f32 {
        0.01
    }
}

// ===========================================================================
// Exporter
// ===========================================================================

/// The export request (`DoExport`'s parameters).
#[derive(Debug, Clone, Default)]
pub struct ExportInput {
    /// The path Max hands the plugin; exactly ten characters are stripped
    /// before the `.animation` suffix is appended.
    pub filename: String,
    /// The scene root node (`ip_->GetRootNode()`).
    pub scene: SceneNode,
    /// `options == SCENE_EXPORT_SELECTED`.
    pub options_selected: bool,
    /// `ip_->GetAnimRange()` as frames (`Start()/GetTicksPerFrame()`,
    /// `End()/GetTicksPerFrame()`).
    pub anim_range: (i32, i32),
    /// `ip_->GetTime()/GetTicksPerFrame()`.
    pub current_frame: i32,
    /// The maxscript `BWAnimationSetting` override section
    /// (`pSettingsOverride`).
    pub override_section: Option<DataSection>,
}

/// The two abort paths of `DoExport`.
#[derive(Debug, Clone, PartialEq)]
pub enum ExportError {
    /// The valid game path check; the message lists the res paths.
    NotInResPath(String),
    /// `validateAnimationIDs` failure; the message lists the sorted
    /// duplicate identifiers, one `Duplicate node name <id>` per line.
    InvalidIds(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportError::NotInResPath(message) | ExportError::InvalidIds(message) => {
                write!(f, "{message}")
            }
        }
    }
}

/// The exporter (`MFXExport::DoExport`), generic over the host seam.
pub struct MfxExporter<I: AnimationIo> {
    io: I,
    cue_track: CueTrack,
    warnings: Vec<String>,
    /// `meshNodes_` of the last export, as flat-scene indices.
    mesh_nodes: Vec<usize>,
    /// `envelopeNodes_` of the last export.
    envelope_nodes: Vec<usize>,
}

impl<I: AnimationIo> MfxExporter<I> {
    pub fn new(io: I) -> Self {
        Self {
            io,
            cue_track: CueTrack::default(),
            warnings: Vec::new(),
            mesh_nodes: Vec::new(),
            envelope_nodes: Vec::new(),
        }
    }

    /// The `trailingLeadingWhitespaces` warnings of the last export.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The cue track accumulated by the last export.
    pub fn cue_track(&self) -> &CueTrack {
        &self.cue_track
    }

    /// The `meshNodes_` classification of the last export.
    pub fn mesh_nodes(&self) -> &[usize] {
        &self.mesh_nodes
    }

    /// The `envelopeNodes_` classification of the last export.
    pub fn envelope_nodes(&self) -> &[usize] {
        &self.envelope_nodes
    }

    /// `MFXExport::preProcess`: one [`MfxNode`] per scene node, include
    /// flags per the node filter (the scene root always passes), the
    /// forced class exclusions, the mesh/envelope split and the cue
    /// harvest (every node's note keys, pre-order, when the cue track is
    /// wanted).
    fn pre_process(&mut self, settings: &ExportSettings, scene: &SceneNode) -> MfxTree {
        let tpf = self.io.ticks_per_frame();
        let mut tree = MfxTree {
            nodes: Vec::new(),
            root: 0,
            flat: Vec::new(),
            mesh_nodes: Vec::new(),
            envelope_nodes: Vec::new(),
        };
        tree.root = self.visit_node(&mut tree, settings, scene, None, tpf);
        tree
    }

    fn visit_node(
        &mut self,
        tree: &mut MfxTree,
        settings: &ExportSettings,
        scene: &SceneNode,
        parent: Option<usize>,
        tpf: i32,
    ) -> usize {
        let mut include = if settings.node_filter == NodeFilter::All || parent.is_none() {
            true
        } else if settings.node_filter == NodeFilter::Selected {
            scene.selected
        } else {
            !scene.hidden
        };

        // The `EvalWorldState(staticFrame)` class exclusions.
        if scene.kind != NodeKind::Object {
            include = false;
        }

        let flat_index = tree.flat.len();

        // The `MFXNode` constructor harvests the note keys for every node
        // when the cue track is wanted.
        if settings.export_cue_track {
            for (ticks, note) in &scene.note_keys {
                self.cue_track.add_cue(
                    *ticks,
                    note,
                    tpf,
                    settings.frame_first,
                    settings.frame_last,
                );
            }
        }

        if include {
            // The physique/skin split: skinned nodes export through the
            // envelope path and lose their animation include flag.
            if scene.skinned {
                tree.envelope_nodes.push(flat_index);
                include = false;
            } else {
                tree.mesh_nodes.push(flat_index);
            }
        }

        let index = tree.nodes.len();
        tree.nodes.push(MfxNode {
            identifier: scene.name.clone(),
            scene: Some(flat_index),
            parent,
            children: Vec::new(),
            include,
            transform: Matrix::identity(),
        });
        tree.flat.push(FlatScene {
            name: scene.name.clone(),
            world: scene.world.clone(),
        });
        if let Some(parent) = parent {
            tree.nodes[parent].children.push(index);
        }

        for child in &scene.children {
            self.visit_node(tree, settings, child, Some(index), tpf);
        }
        index
    }

    /// `MFXExport::DoExport`.
    pub fn do_export(&mut self, input: &ExportInput) -> Result<(), ExportError> {
        self.warnings.clear();

        // Force the extension: exactly ten characters are stripped before
        // ".animation" is appended. A shorter name strips nothing (the
        // reference's `substr` would throw); a cut that would land inside
        // a UTF-8 sequence strips nothing either.
        let cut = input.filename.len().saturating_sub(10);
        let stem = if input.filename.is_char_boundary(cut) {
            &input.filename[..cut]
        } else {
            input.filename.as_str()
        };
        let filename = format!("{stem}.animation");

        // Check if the filename is in the right res path.
        let name2 = self.io.dissolve_filename(&filename);
        if name2.len() == filename.len() || name2.rfind(['/', '\\']).is_none() {
            let mut error_string = format!(
                "The exported file \"{}\" is not in a valid game path.\n",
                filename
            );
            error_string.push_str("Need to export to a *subfolder* of:");
            for path in self.io.res_paths() {
                error_string.push('\n');
                error_string.push_str(&path);
            }
            return Err(ExportError::NotInResPath(error_string));
        }

        let mut settings = ExportSettings::default();
        settings.set_frame_range(input.anim_range.0, input.anim_range.1);

        let cfg_filename = self.io.cfg_filename();
        let settings_filename = format!("{}.animationsettings", remove_extension(&filename));

        settings.read_settings(self.io.read_settings_xml(&cfg_filename).as_ref());
        settings.read_settings(self.io.read_settings_xml(&settings_filename).as_ref());
        settings.read_settings(input.override_section.as_ref());

        settings.static_frame = input.current_frame;

        // `displayDialog` is the host GUI face; the port runs
        // prompt-suppressed, which takes the dialog's IDOK path.

        if input.options_selected {
            settings.node_filter = NodeFilter::Selected;
        } else {
            settings.node_filter = NodeFilter::Visible;
        }

        let mut cfg_root = DataSection::new("root");
        settings.write_settings(Some(&mut cfg_root));
        self.io.write_settings_xml(&cfg_filename, &cfg_root);

        // clear any old cue track as plugins is kept loaded and CueTrack
        // is a singleton
        if settings.export_cue_track {
            self.cue_track.clear();
        }

        let mut tree = self.pre_process(&settings, &input.scene);
        self.mesh_nodes = tree.mesh_nodes.clone();
        self.envelope_nodes = tree.envelope_nodes.clone();

        // exportMeshes()/exportEnvelopes() are the Max-host companion
        // visual write; the port keeps the classification on the tree.

        if !settings.reference_nodes_file.is_empty() {
            let resolved = self.io.dissolve_filename(&settings.reference_nodes_file);
            if let Some(visual) = self.io.open_section(&resolved) {
                // `readReferenceHierarchy` starts at the root "node"
                // section; a visual without one skips the re-parent (the
                // reference dereferences NULL there).
                if let Some(root_node) = visual.open_section("node") {
                    let mut pairs = Vec::new();
                    read_reference_hierarchy(root_node, &mut pairs);

                    // The reference's `StringHashMap` iteration order is
                    // unspecified; the port applies first-insertion order
                    // with last-wins parents.
                    let mut order: Vec<String> = Vec::new();
                    let mut parents: HashMap<String, String> = HashMap::new();
                    for (child, parent) in pairs {
                        if !parents.contains_key(&child) {
                            order.push(child.clone());
                        }
                        parents.insert(child, parent);
                    }
                    for child in order {
                        let parent = parents[&child].clone();
                        if let (Some(p), Some(c)) = (tree.find(&parent), tree.find(&child)) {
                            tree.reparent(c, p);
                        }
                    }
                }
            }
        }

        // Order the immediate children of mfxRoot_ in decending order
        // based on their respective child count.
        tree.sort_root_children();

        tree.munge_included();

        // Check if we have any duplicate node id's or morph target ids
        if settings.export_node_animation {
            let duplicates = tree.duplicate_names();
            if !duplicates.is_empty() {
                let mut error_message = String::new();
                for duplicate in &duplicates {
                    error_message.push_str("Duplicate node name ");
                    error_message.push_str(duplicate);
                    error_message.push('\n');
                }
                return Err(ExportError::InvalidIds(error_message));
            }
        }

        let mut anim_name = filename.clone();
        // `find_last_of("\\/") > 0` skips a leading separator and falls
        // through (whole string) when none is found.
        if let Some(i) = anim_name.rfind(['\\', '/'])
            && i > 0
        {
            anim_name = anim_name[i + 1..].to_string();
        }
        if let Some(i) = anim_name.rfind('.')
            && i > 0
        {
            anim_name.truncate(i);
        }

        let mut n_channels: i32 = 0;
        if settings.export_node_animation {
            n_channels += tree.n_included_nodes() as i32;
        }
        if settings.export_cue_track && self.cue_track.has_cues() {
            n_channels += 1;
        }

        if n_channels > 0 {
            let mut ci = CompressionInfo::default();

            // Try to read in compression amounts
            if let Some(existing) = self.io.read_animation(&filename) {
                ci = harvest_compression_info(&existing);
            }

            let mut out = Vec::new();
            w_f32(
                &mut out,
                (settings.frame_last - settings.frame_first) as f32,
            );
            w_str(&mut out, &anim_name);
            w_str(&mut out, &anim_name);
            w_i32(&mut out, n_channels);

            if settings.export_node_animation {
                // The root loses its max node and exports as the identity
                // "Scene Root" channel.
                tree.nodes[tree.root].scene = None;
                tree.nodes[tree.root].identifier = "Scene Root".to_string();
                tree.export_animation(
                    tree.root,
                    &mut out,
                    &ci,
                    &settings,
                    self.io.unit_scale(),
                    &mut self.warnings,
                    None,
                );
            }

            if settings.export_cue_track {
                self.cue_track
                    .write_file(&mut out, self.io.ticks_per_frame());
            }

            if self.io.write_animation(&filename, &out) {
                self.io.delete_file(&settings_filename);
                let mut settings_root = DataSection::new("root");
                settings.write_settings(Some(&mut settings_root));
                self.io
                    .write_settings_xml(&settings_filename, &settings_root);
            }
        }

        Ok(())
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- the host seam ---

    #[derive(Default)]
    struct TestIo {
        res_root: String,
        res_paths: Vec<String>,
        sections: HashMap<String, DataSection>,
        existing_animations: HashMap<String, Vec<u8>>,
        written_animations: HashMap<String, Vec<u8>>,
        written_settings: Vec<(String, DataSection)>,
        deleted: Vec<String>,
        ticks_per_frame: i32,
        unit_scale: f32,
        fail_write: bool,
        /// Dissolve into a subfolder of the res root regardless of the
        /// prefix (`chars/<name>`), for names the strip would leave bare.
        subfolder: bool,
    }

    impl TestIo {
        fn new() -> Self {
            Self {
                res_root: "/res/".to_string(),
                res_paths: vec!["C:/game/res".to_string()],
                ticks_per_frame: 160,
                unit_scale: 0.01,
                ..Default::default()
            }
        }

        fn with_section(mut self, filename: &str, section: DataSection) -> Self {
            self.sections.insert(filename.to_string(), section);
            self
        }

        fn with_existing_animation(mut self, filename: &str, data: &[u8]) -> Self {
            self.existing_animations
                .insert(filename.to_string(), data.to_vec());
            self
        }
    }

    impl AnimationIo for TestIo {
        fn dissolve_filename(&self, filename: &str) -> String {
            if self.subfolder {
                format!("chars/{}", filename.trim_start_matches('/'))
            } else {
                filename
                    .strip_prefix(&self.res_root)
                    .unwrap_or(filename)
                    .to_string()
            }
        }

        fn res_paths(&self) -> Vec<String> {
            self.res_paths.clone()
        }

        fn open_section(&self, filename: &str) -> Option<DataSection> {
            self.sections.get(filename).cloned()
        }

        fn read_settings_xml(&self, filename: &str) -> Option<DataSection> {
            self.sections.get(filename).cloned()
        }

        fn write_settings_xml(&mut self, filename: &str, root: &DataSection) {
            self.written_settings
                .push((filename.to_string(), root.clone()));
        }

        fn read_animation(&self, filename: &str) -> Option<Vec<u8>> {
            self.existing_animations.get(filename).cloned()
        }

        fn write_animation(&mut self, filename: &str, bytes: &[u8]) -> bool {
            if self.fail_write {
                return false;
            }
            self.written_animations
                .insert(filename.to_string(), bytes.to_vec());
            true
        }

        fn delete_file(&mut self, filename: &str) {
            self.deleted.push(filename.to_string());
        }

        fn ticks_per_frame(&self) -> i32 {
            self.ticks_per_frame
        }

        fn unit_scale(&self) -> f32 {
            self.unit_scale
        }
    }

    // --- builders ---

    fn identity_frames(n: usize) -> Vec<Matrix> {
        vec![Matrix::identity(); n]
    }

    fn plain_node(name: &str, frames: usize) -> SceneNode {
        SceneNode {
            name: name.to_string(),
            world: identity_frames(frames),
            ..Default::default()
        }
    }

    fn translated_node(name: &str, x: f32, y: f32, z: f32) -> SceneNode {
        let mut m = Matrix::identity();
        m.m[3][0] = x;
        m.m[3][1] = y;
        m.m[3][2] = z;
        SceneNode {
            name: name.to_string(),
            world: vec![m],
            ..Default::default()
        }
    }

    /// A settings file section carrying the given key values.
    fn settings_section(allow_scale: Option<bool>, export_node: Option<bool>) -> DataSection {
        let mut root = DataSection::new("root");
        if let Some(v) = allow_scale {
            root.write_bool("allowScale", v);
        }
        if let Some(v) = export_node {
            root.write_bool("exportNodeAnimation", v);
        }
        root
    }

    // --- a reader over our own output ---

    struct TestReader<'a> {
        data: &'a [u8],
        pos: usize,
    }

    impl<'a> TestReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self { data, pos: 0 }
        }

        fn f32(&mut self) -> f32 {
            let v = f32::from_le_bytes(self.data[self.pos..self.pos + 4].try_into().unwrap());
            self.pos += 4;
            v
        }

        fn i32(&mut self) -> i32 {
            let v = i32::from_le_bytes(self.data[self.pos..self.pos + 4].try_into().unwrap());
            self.pos += 4;
            v
        }

        fn string(&mut self) -> String {
            let len = self.i32() as usize;
            let s = String::from_utf8_lossy(&self.data[self.pos..self.pos + len]).into_owned();
            self.pos += len;
            s
        }

        fn vec3(&mut self) -> Vector3 {
            Vector3::new(self.f32(), self.f32(), self.f32())
        }

        fn vec3_seq(&mut self) -> Vec<(f32, Vector3)> {
            let count = self.i32();
            (0..count)
                .map(|_| {
                    let t = self.f32();
                    (t, self.vec3())
                })
                .collect()
        }

        fn quat_seq(&mut self) -> Vec<(f32, Quaternion)> {
            let count = self.i32();
            (0..count)
                .map(|_| {
                    let t = self.f32();
                    let q = Quaternion::new(self.f32(), self.f32(), self.f32(), self.f32());
                    (t, q)
                })
                .collect()
        }

        fn i32_seq(&mut self) -> Vec<i32> {
            let count = self.i32();
            (0..count).map(|_| self.i32()).collect()
        }

        /// Skip past the file header (duration, name, name, channel count).
        fn skip_header(&mut self) -> i32 {
            let _ = self.f32();
            let _ = self.string();
            let _ = self.string();
            self.i32()
        }
    }

    /// Parse a whole `.animation`: the header plus every channel.
    #[allow(clippy::type_complexity)]
    fn read_animation(
        bytes: &[u8],
    ) -> (
        f32,
        String,
        String,
        i32,
        Vec<(
            i32,
            String,
            Vec<(f32, Vector3)>,
            Vec<(f32, Vector3)>,
            Vec<(f32, Quaternion)>,
            Vec<i32>,
        )>,
    ) {
        let mut r = TestReader::new(bytes);
        let total = r.f32();
        let name = r.string();
        let name2 = r.string();
        let count = r.i32();
        let mut channels = Vec::new();
        for _ in 0..count {
            let channel_type = r.i32();
            let identifier = r.string();
            if channel_type == 4 {
                let _ = r.f32();
                let _ = r.f32();
                let _ = r.f32();
            }
            let scales = r.vec3_seq();
            let positions = r.vec3_seq();
            let rotations = r.quat_seq();
            let bound = r.i32_seq();
            let _ = r.i32_seq();
            let _ = r.i32_seq();
            channels.push((
                channel_type,
                identifier,
                scales,
                positions,
                rotations,
                bound,
            ));
        }
        (total, name, name2, count, channels)
    }

    fn export(io: TestIo, input: &ExportInput) -> (MfxExporter<TestIo>, Vec<u8>) {
        let mut exporter = MfxExporter::new(io);
        exporter.do_export(input).expect("export succeeds");
        let bytes = exporter
            .io
            .written_animations
            .values()
            .next()
            .cloned()
            .expect("an animation was written");
        (exporter, bytes)
    }

    // --- utility math ---

    #[test]
    fn rearrange_swaps_axes_and_keeps_the_translation_row() {
        // A matrix with distinct entries: rows 0..2 built from 1..9, the
        // translation row distinct again.
        let mut m = Matrix::identity();
        let values = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]];
        for (row, row_vals) in values.iter().enumerate() {
            for (col, v) in row_vals.iter().enumerate() {
                m.m[row][col] = *v;
            }
        }
        m.m[3] = [0.5, 1.5, 2.5, 1.0];

        let out = rearrange_matrix(&m, false);

        // Column swap first (col1 <- col2, col2 <- col1), then row swap
        // 1 <-> 2: out[i][j] = m[swap(i)][swap(j)].
        let expected = [[1.0, 3.0, 2.0], [7.0, 9.0, 8.0], [4.0, 6.0, 5.0]];
        for (row, row_vals) in expected.iter().enumerate() {
            for (col, v) in row_vals.iter().enumerate() {
                assert_eq!(out.m[row][col], *v);
            }
        }
        // The translation row survives the whole rearrange (pinned by the
        // crab visual/animation pair).
        assert_eq!(out.m[3], [0.5, 1.5, 2.5, 1.0]);
    }

    #[test]
    fn rearrange_legacy_negates_the_xor_pattern_only() {
        let mut m = Matrix::identity();
        for (row, row_vals) in m.m[..3].iter_mut().enumerate() {
            for (col, value) in row_vals[..3].iter_mut().enumerate() {
                *value = (row * 3 + col) as f32 + 1.0;
            }
        }
        m.m[3] = [7.0, 8.0, 9.0, 1.0];

        let out = rearrange_matrix(&m, true);

        for (row, row_vals) in out.m[..3].iter().enumerate() {
            for (col, value) in row_vals[..3].iter().enumerate() {
                let expected = if (row == 0 || row == 2) ^ (col == 0 || col == 2) {
                    -1.0
                } else {
                    1.0
                };
                assert_eq!(
                    *value,
                    m_swap(&m, row, col) * expected,
                    "sign mismatch at ({row},{col})"
                );
            }
        }
        // Row 3 is never negated.
        assert_eq!(out.m[3], [7.0, 8.0, 9.0, 1.0]);
    }

    /// `S·m·S` reference: the axis-1/axis-2 swap the rearrange performs.
    fn m_swap(m: &Matrix, row: usize, col: usize) -> f32 {
        let swap = |i: usize| {
            if i == 1 {
                2
            } else if i == 2 {
                1
            } else {
                i
            }
        };
        m.m[swap(row)][swap(col)]
    }

    #[test]
    fn normalise_renormalises_rows_and_passes_the_translation_through() {
        let mut m = Matrix::identity();
        m.m[0] = [2.0, 0.0, 0.0, 0.0];
        m.m[1] = [0.0, 4.0, 0.0, 0.0];
        m.m[2] = [0.0, 0.0, 8.0, 0.0];
        m.m[3] = [3.0, 4.0, 5.0, 1.0];

        let out = normalise_matrix(&m);

        assert_eq!(out.m[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(out.m[1], [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(out.m[2], [0.0, 0.0, 1.0, 0.0]);
        assert_eq!(out.m[3], [3.0, 4.0, 5.0, 1.0]);
    }

    #[test]
    fn trailing_leading_whitespaces_checks_both_ends() {
        assert!(trailing_leading_whitespaces(" x"));
        assert!(trailing_leading_whitespaces("x "));
        assert!(!trailing_leading_whitespaces("x"));
        assert!(!trailing_leading_whitespaces(""));
    }

    #[test]
    fn blend_decompose_of_identity_is_the_identity_key() {
        let (rotation, scale, translation) = blend_decompose(&Matrix::identity());
        assert_eq!(rotation, Quaternion::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(scale, Vector3::new(1.0, 1.0, 1.0));
        assert_eq!(translation, Vector3::new(0.0, 0.0, 0.0));
    }

    #[test]
    fn blend_decompose_of_a_scaled_translation_and_rotation() {
        // Rotation by 90 degrees about z, scaled 2 in x, translated.
        let mut m = Matrix::identity();
        m.m[0] = [0.0, 2.0, 0.0, 0.0];
        m.m[1] = [-2.0, 0.0, 0.0, 0.0];
        m.m[2] = [0.0, 0.0, 1.0, 0.0];
        m.m[3] = [1.0, 2.0, 3.0, 1.0];

        let (rotation, scale, translation) = blend_decompose(&m);
        assert_eq!(scale, Vector3::new(2.0, 2.0, 1.0));
        assert_eq!(translation, Vector3::new(1.0, 2.0, 3.0));
        // The rotation is the plain 90-degree z quaternion.
        let expected =
            Quaternion::from_angle_axis(std::f32::consts::FRAC_PI_2, &Vector3::new(0.0, 0.0, 1.0));
        let (mut rotation, mut expected) = (rotation, expected);
        rotation.normalise();
        expected.normalise();
        let dot = rotation.dot_product(&expected).abs();
        assert!(dot > 0.999999, "rotation {rotation:?} vs {expected:?}");
    }

    #[test]
    fn blend_decompose_of_a_mirrored_matrix_flips_the_z_scale() {
        // Left-handed: row 2 negated relative to a right-handed rotation.
        let mut m = Matrix::identity();
        m.m[0] = [0.0, 1.0, 0.0, 0.0];
        m.m[1] = [-1.0, 0.0, 0.0, 0.0];
        m.m[2] = [0.0, 0.0, -1.0, 0.0];

        let (rotation, scale, translation) = blend_decompose(&m);
        assert_eq!(scale, Vector3::new(1.0, 1.0, -1.0));
        assert_eq!(translation, Vector3::new(0.0, 0.0, 0.0));
        // Determinant -1: the rotation part stays a unit quaternion.
        assert!((rotation.length() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn remove_extension_strips_the_last_dot() {
        assert_eq!(
            remove_extension("/res/chars/demo.animation"),
            "/res/chars/demo"
        );
        assert_eq!(remove_extension("noext"), "noext");
    }

    // --- settings ---

    #[test]
    fn settings_round_trip_the_five_keys() {
        let settings = ExportSettings {
            allow_scale: true,
            export_node_animation: false,
            export_cue_track: true,
            use_legacy_orientation: true,
            reference_nodes_file: "crab.visual".to_string(),
            ..Default::default()
        };

        let mut root = DataSection::new("root");
        assert!(settings.write_settings(Some(&mut root)));

        let mut loaded = ExportSettings::default();
        assert!(loaded.read_settings(Some(&root)));
        assert_eq!(loaded, settings);
    }

    #[test]
    fn settings_reads_keep_current_defaults_for_missing_keys() {
        let mut settings = ExportSettings {
            allow_scale: true,
            reference_nodes_file: "x.visual".to_string(),
            ..Default::default()
        };

        let root = DataSection::new("root");
        assert!(settings.read_settings(Some(&root)));
        // Nothing changed.
        assert_eq!(
            settings,
            ExportSettings {
                allow_scale: true,
                reference_nodes_file: "x.visual".to_string(),
                ..Default::default()
            }
        );

        // A missing file (None section) reads false and changes nothing.
        let mut settings = ExportSettings::default();
        assert!(!settings.read_settings(None));
        assert_eq!(settings, ExportSettings::default());
    }

    // --- cue track ---

    #[test]
    fn cue_track_gates_prefixes_and_dedups() {
        let mut cues = CueTrack::default();
        cues.add_cue(0, "cue:boom", 160, 0, 10);
        cues.add_cue(5 * 160, "sound:hit", 160, 0, 10);
        // Outside the frame range: skipped.
        cues.add_cue(11 * 160, "cue:late", 160, 0, 10);
        cues.add_cue(-160, "cue:early", 160, 0, 10);
        // No colon / unknown prefix: skipped.
        cues.add_cue(160, "plain", 160, 0, 10);
        cues.add_cue(160, "boom:cue", 160, 0, 10);
        // Duplicate at the same time: dropped; a different text is kept.
        cues.add_cue(0, "cue:boom", 160, 0, 10);
        cues.add_cue(0, "cue:other", 160, 0, 10);

        let map = cues.cues();
        assert_eq!(map.len(), 2);
        assert_eq!(map[&0], ["cboom", "cother"]);
        assert_eq!(map[&(5 * 160)], ["shit"]);
        assert!(cues.has_cues());
    }

    #[test]
    fn cue_track_write_file_bytes_and_the_empty_skip() {
        let mut cues = CueTrack::default();
        cues.add_cue(2 * 160, "cue:first", 160, 1, 9);
        cues.add_cue(4 * 160, "sound:two", 160, 1, 9);

        let mut out = Vec::new();
        cues.write_file(&mut out, 160);

        let mut r = TestReader::new(&out);
        assert_eq!(r.i32(), 6);
        assert_eq!(r.i32(), 2);
        // First: time adjusted by the first frame (2-1 = 1.0).
        assert_eq!(r.f32(), 1.0);
        assert_eq!(r.string(), "cfirst");
        assert_eq!(r.i32(), 0);
        assert_eq!(r.f32(), 3.0);
        assert_eq!(r.string(), "stwo");
        assert_eq!(r.i32(), 0);

        // No cues: not even the type marker.
        let mut empty = Vec::new();
        CueTrack::default().write_file(&mut empty, 160);
        assert!(empty.is_empty());
    }

    // --- compression harvest ---

    #[test]
    fn harvest_reads_a_type_four_first_channel() {
        let mut data = Vec::new();
        w_f32(&mut data, 10.0);
        w_str(&mut data, "old");
        w_str(&mut data, "old");
        w_i32(&mut data, 2);
        w_i32(&mut data, 4);
        w_str(&mut data, "ch0");
        w_f32(&mut data, 0.5);
        w_f32(&mut data, 0.25);
        w_f32(&mut data, 0.125);

        let ci = harvest_compression_info(&data);
        assert!(ci.specify_amounts);
        assert_eq!(ci.scale_compression_error, 0.5);
        assert_eq!(ci.position_compression_error, 0.25);
        assert_eq!(ci.rotation_compression_error, 0.125);
    }

    #[test]
    fn harvest_defaults_for_type_one_and_short_files() {
        let mut data = Vec::new();
        w_f32(&mut data, 1.0);
        w_str(&mut data, "a");
        w_str(&mut data, "a");
        w_i32(&mut data, 1);
        w_i32(&mut data, 1);
        w_str(&mut data, "ch0");
        assert_eq!(harvest_compression_info(&data), CompressionInfo::default());

        // Truncated header: zeros, not garbage.
        assert_eq!(harvest_compression_info(&[]), CompressionInfo::default());
        let ci = harvest_compression_info(&[0u8; 6]);
        assert_eq!(ci, CompressionInfo::default());
    }

    // --- the export flows ---

    #[test]
    fn identity_scene_writes_the_golden_bytes() {
        let input = ExportInput {
            filename: "/res/chars/demo.animation".to_string(),
            scene: plain_node("Scene", 3),
            anim_range: (0, 2),
            ..Default::default()
        };
        let (_, bytes) = export(TestIo::new(), &input);

        let mut expected = Vec::new();
        w_f32(&mut expected, 2.0);
        w_str(&mut expected, "demo");
        w_str(&mut expected, "demo");
        w_i32(&mut expected, 1);
        // The root channel: identity keys, times 0..2, bound table 1..3
        // three times.
        w_i32(&mut expected, 1);
        w_str(&mut expected, "Scene Root");
        w_i32(&mut expected, 3);
        for t in 0..3 {
            w_f32(&mut expected, t as f32);
            w_f32(&mut expected, 1.0);
            w_f32(&mut expected, 1.0);
            w_f32(&mut expected, 1.0);
        }
        w_i32(&mut expected, 3);
        for t in 0..3 {
            w_f32(&mut expected, t as f32);
            for _ in 0..3 {
                w_f32(&mut expected, 0.0);
            }
        }
        w_i32(&mut expected, 3);
        for t in 0..3 {
            w_f32(&mut expected, t as f32);
            for v in [0.0, 0.0, 0.0, 1.0] {
                w_f32(&mut expected, v);
            }
        }
        w_i32(&mut expected, 3);
        for v in 1..=3 {
            w_i32(&mut expected, v);
        }
        w_i32(&mut expected, 3);
        for v in 1..=3 {
            w_i32(&mut expected, v);
        }
        w_i32(&mut expected, 3);
        for v in 1..=3 {
            w_i32(&mut expected, v);
        }

        assert_eq!(bytes, expected);
    }

    #[test]
    fn root_children_are_sorted_descending_and_channels_follow() {
        let mut tail = plain_node("Tail", 1);
        tail.world = vec![Matrix::identity()];
        let mut spine = plain_node("Spine", 1);
        spine.children.push(tail);
        let mut scene = plain_node("Scene", 1);
        scene.children = vec![plain_node("Left", 1), spine, plain_node("Right", 1)];

        let input = ExportInput {
            filename: "/res/chars/order.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };
        let (_, bytes) = export(TestIo::new(), &input);

        // Scene Root, then Spine (1 child) ahead of the stable Left,
        // Right, with Tail directly after Spine.
        let mut r = TestReader::new(&bytes);
        assert_eq!(r.f32(), 0.0);
        assert_eq!(r.string(), "order");
        assert_eq!(r.string(), "order");
        assert_eq!(r.i32(), 5);
        let identifiers: Vec<String> = (0..5)
            .map(|_| {
                let ty = r.i32();
                assert_eq!(ty, 1);
                let name = r.string();
                // Skip the rest of the channel body.
                let _ = (r.vec3_seq(), r.vec3_seq(), r.quat_seq());
                let _ = (r.i32_seq(), r.i32_seq(), r.i32_seq());
                name
            })
            .collect();
        assert_eq!(
            identifiers,
            ["Scene Root", "Spine", "Tail", "Left", "Right"]
        );
    }

    #[test]
    fn munge_re_includes_the_ancestors_of_included_nodes() {
        let hand = plain_node("Hand", 1);
        let mut arm = plain_node("Arm", 1);
        arm.hidden = true;
        arm.children.push(hand);
        let mut scene = plain_node("Scene", 1);
        scene.children.push(arm);

        let input = ExportInput {
            filename: "/res/chars/munge.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };
        let (_, bytes) = export(TestIo::new(), &input);

        let mut r = TestReader::new(&bytes);
        assert_eq!(r.skip_header(), 3);
        for expected in ["Scene Root", "Arm", "Hand"] {
            assert_eq!(r.i32(), 1);
            assert_eq!(r.string(), expected);
            // Skip the rest of the channel.
            let _ = r.vec3_seq();
            let _ = r.vec3_seq();
            let _ = r.quat_seq();
            let _ = r.i32_seq();
            let _ = r.i32_seq();
            let _ = r.i32_seq();
        }
    }

    #[test]
    fn forced_kinds_are_excluded_even_when_visible() {
        let mut scene = plain_node("Scene", 1);
        let mut excluded = Vec::new();
        for (name, kind) in [
            ("Cam", NodeKind::Camera),
            ("Lamp", NodeKind::Light),
            ("Shape", NodeKind::Shape),
            ("Tgt", NodeKind::Target),
            ("PView", NodeKind::ParticleView),
        ] {
            let mut node = plain_node(name, 1);
            node.kind = kind;
            excluded.push(node);
        }
        scene.children = excluded;

        let input = ExportInput {
            filename: "/res/chars/kinds.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };
        let (exporter, bytes) = export(TestIo::new(), &input);

        let mut r = TestReader::new(&bytes);
        assert_eq!(r.skip_header(), 1);
        assert_eq!(r.i32(), 1);
        assert_eq!(r.string(), "Scene Root");
        // None of the excluded kinds made the mesh list.
        assert_eq!(exporter.mesh_nodes(), &[0]);
        assert!(exporter.envelope_nodes().is_empty());
    }

    #[test]
    fn selected_filter_keeps_only_selected_nodes() {
        let mut scene = plain_node("Scene", 1);
        let mut picked = plain_node("Picked", 1);
        picked.selected = true;
        scene.children = vec![picked, plain_node("Skipped", 1)];

        let input = ExportInput {
            filename: "/res/chars/sel.animation".to_string(),
            scene,
            anim_range: (0, 0),
            options_selected: true,
            ..Default::default()
        };
        let (_, bytes) = export(TestIo::new(), &input);

        let mut r = TestReader::new(&bytes);
        assert_eq!(r.skip_header(), 2);
        // Scene Root channel, consumed wholesale.
        assert_eq!(r.i32(), 1);
        assert_eq!(r.string(), "Scene Root");
        let _ = (r.vec3_seq(), r.vec3_seq(), r.quat_seq());
        let _ = (r.i32_seq(), r.i32_seq(), r.i32_seq());
        assert_eq!(r.i32(), 1);
        assert_eq!(r.string(), "Picked");
    }

    #[test]
    fn skinned_nodes_go_to_the_envelope_list_and_lose_their_channel() {
        let mut scene = plain_node("Scene", 1);
        let mut skin = plain_node("Skin", 1);
        skin.skinned = true;
        scene.children = vec![plain_node("Mesh", 1), skin];

        let input = ExportInput {
            filename: "/res/chars/split.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };
        let (exporter, bytes) = export(TestIo::new(), &input);

        // Flat indices: Scene 0, Mesh 1, Skin 2. The root itself is
        // included and unskinned, so the reference pushes it to
        // meshNodes_ first (Scene, Mesh), then Skin to envelopeNodes_.
        assert_eq!(exporter.mesh_nodes(), &[0, 1]);
        assert_eq!(exporter.envelope_nodes(), &[2]);

        let mut r = TestReader::new(&bytes);
        assert_eq!(r.skip_header(), 2); // Scene Root + Mesh
        // Scene Root channel, consumed wholesale.
        assert_eq!(r.i32(), 1);
        assert_eq!(r.string(), "Scene Root");
        let _ = (r.vec3_seq(), r.vec3_seq(), r.quat_seq());
        let _ = (r.i32_seq(), r.i32_seq(), r.i32_seq());
        assert_eq!(r.i32(), 1);
        assert_eq!(r.string(), "Mesh");
    }

    #[test]
    fn reference_hierarchy_re_parents_the_channel_transforms() {
        // Scene: Scene -> Arm (x=4) -> Hand (world x=8, y=4, z=0.5).
        let hand = translated_node("Hand", 8.0, 4.0, 0.5);
        let mut arm = translated_node("Arm", 4.0, 0.0, 0.0);
        arm.children.push(hand);
        let mut scene = plain_node("Scene", 1);
        scene.children.push(arm);

        // The reference visual: Hand belongs to Scene.
        let mut visual = DataSection::new("visual");
        let root_node = visual.add_child("node");
        root_node.write_string("identifier", "Scene");
        let hand_node = root_node.add_child("node");
        hand_node.write_string("identifier", "Hand");

        // The settings file points at the reference hierarchy.
        let mut file = DataSection::new("root");
        file.write_string("referenceNodesFile", "chars/crab.visual");

        let io = TestIo::new()
            .with_section("chars/crab.visual", visual)
            .with_section("/res/chars/crab_walk.animationsettings", file);
        let input = ExportInput {
            filename: "/res/chars/crab_walk.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };
        let (_, bytes) = export(io, &input);

        let (_, name1, name2, _, channels) = read_animation(&bytes);
        assert_eq!(name1, "crab_walk");
        assert_eq!(name2, "crab_walk");
        assert_eq!(channels.len(), 3);
        assert_eq!(
            channels.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(),
            ["Scene Root", "Arm", "Hand"]
        );

        // Arm stays under the root: x=4 times the unit scale. Hand is
        // re-parented to the root, so its world translation reaches the
        // channel directly instead of the (8-4, 4, 0.5) relative offset.
        assert_eq!(channels[1].3[0].1, Vector3::new(0.04, 0.0, 0.0));
        assert_eq!(channels[2].3[0].1, Vector3::new(0.08, 0.04, 0.005));
    }

    #[test]
    fn export_node_animation_off_writes_only_the_cue_channel() {
        let mut scene = plain_node("Scene", 6);
        scene.note_keys = vec![
            (0, "cue:boom".to_string()),
            (5 * 160, "cue:hit".to_string()),
        ];
        let input = ExportInput {
            filename: "/res/chars/cues.animation".to_string(),
            scene,
            anim_range: (0, 5),
            ..Default::default()
        };
        // The animationsettings file turns the node animation off and the
        // cue track on.
        let mut file = DataSection::new("root");
        file.write_bool("exportNodeAnimation", false);
        file.write_bool("exportCueTrack", true);
        let io = TestIo::new().with_section("/res/chars/cues.animationsettings", file);

        let (exporter, bytes) = export(io, &input);
        assert!(exporter.cue_track().has_cues());

        let mut r = TestReader::new(&bytes);
        assert_eq!(r.f32(), 5.0);
        assert_eq!(r.string(), "cues");
        assert_eq!(r.string(), "cues");
        assert_eq!(r.i32(), 1);
        assert_eq!(r.i32(), 6);
        assert_eq!(r.i32(), 2);
        assert_eq!(r.f32(), 0.0);
        assert_eq!(r.string(), "cboom");
        assert_eq!(r.i32(), 0);
        assert_eq!(r.f32(), 5.0);
        assert_eq!(r.string(), "chit");
        assert_eq!(r.i32(), 0);
    }

    #[test]
    fn zero_channels_writes_no_file() {
        let mut file = DataSection::new("root");
        file.write_bool("exportNodeAnimation", false);
        let io = TestIo::new().with_section("/res/chars/none.animationsettings", file);
        let input = ExportInput {
            filename: "/res/chars/none.animation".to_string(),
            scene: plain_node("Scene", 1),
            anim_range: (0, 0),
            ..Default::default()
        };

        let mut exporter = MfxExporter::new(io);
        exporter.do_export(&input).expect("Ok with no channels");
        assert!(exporter.io.written_animations.is_empty());
        // And no settings rewrite either (it sits inside the channel
        // block).
        assert!(exporter.io.deleted.is_empty());
    }

    #[test]
    fn compression_errors_are_harvested_into_type_four_channels() {
        let mut existing = Vec::new();
        w_f32(&mut existing, 3.0);
        w_str(&mut existing, "old");
        w_str(&mut existing, "old");
        w_i32(&mut existing, 1);
        w_i32(&mut existing, 4);
        w_str(&mut existing, "ch0");
        w_f32(&mut existing, 0.5);
        w_f32(&mut existing, 0.25);
        w_f32(&mut existing, 0.125);

        let input = ExportInput {
            filename: "/res/chars/demo.animation".to_string(),
            scene: plain_node("Scene", 1),
            anim_range: (0, 0),
            ..Default::default()
        };
        let io = TestIo::new().with_existing_animation("/res/chars/demo.animation", &existing);
        let (_, bytes) = export(io, &input);

        let (_, _, _, _, channels) = read_animation(&bytes);
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].0, 4);
        assert_eq!(channels[0].1, "Scene Root");

        // The three harvested errors sit right behind the identifier.
        let mut r = TestReader::new(&bytes);
        let _ = r.f32();
        let _ = r.string();
        let _ = r.string();
        let _ = r.i32();
        assert_eq!(r.i32(), 4);
        let _ = r.string();
        assert_eq!(r.f32(), 0.5);
        assert_eq!(r.f32(), 0.25);
        assert_eq!(r.f32(), 0.125);
    }

    #[test]
    fn settings_are_written_to_cfg_and_the_settings_file_is_replaced() {
        let input = ExportInput {
            filename: "/res/chars/demo.animation".to_string(),
            scene: plain_node("Scene", 1),
            anim_range: (0, 0),
            ..Default::default()
        };
        let (exporter, _) = export(TestIo::new(), &input);

        // The cfg file is written right after the read chain (with the
        // merged settings), the animationsettings file is deleted and
        // rewritten after the animation.
        assert_eq!(exporter.io.written_settings[0].0, "animationexporter.cfg");
        assert!(
            !exporter.io.written_settings[0]
                .1
                .read_bool("allowScale", true)
        );
        assert_eq!(
            exporter.io.deleted,
            ["/res/chars/demo.animationsettings".to_string()]
        );
        assert_eq!(
            exporter.io.written_settings[1].0,
            "/res/chars/demo.animationsettings"
        );
    }

    #[test]
    fn settings_chain_merges_cfg_settings_file_and_override() {
        let mut cfg = DataSection::new("root");
        cfg.write_bool("exportNodeAnimation", false);
        cfg.write_string("referenceNodesFile", "a.visual");
        let mut file = DataSection::new("root");
        file.write_bool("exportNodeAnimation", true);
        file.write_string("referenceNodesFile", "b.visual");
        let mut over = DataSection::new("settings");
        over.write_string("referenceNodesFile", "c.visual");
        over.write_bool("useLegacyOrientation", true);

        let io = TestIo::new()
            .with_section("animationexporter.cfg", cfg)
            .with_section("/res/chars/demo.animationsettings", file);
        let input = ExportInput {
            filename: "/res/chars/demo.animation".to_string(),
            scene: plain_node("Scene", 1),
            anim_range: (0, 0),
            override_section: Some(over),
            ..Default::default()
        };
        let (exporter, _) = export(io, &input);

        // The cfg write-back carries the merged result: node animation
        // re-enabled by the settings file, the reference file and the
        // legacy flag from the maxscript override.
        let merged = &exporter.io.written_settings[0].1;
        assert!(merged.read_bool("exportNodeAnimation", false));
        assert_eq!(merged.read_string("referenceNodesFile", ""), "c.visual");
        assert!(merged.read_bool("useLegacyOrientation", false));
    }

    #[test]
    fn a_failed_animation_write_skips_the_settings_rewrite() {
        let io = TestIo {
            fail_write: true,
            ..TestIo::new()
        };
        let input = ExportInput {
            filename: "/res/chars/demo.animation".to_string(),
            scene: plain_node("Scene", 1),
            anim_range: (0, 0),
            ..Default::default()
        };
        let mut exporter = MfxExporter::new(io);
        exporter.do_export(&input).expect("still Ok");

        // Only the cfg write happened.
        assert_eq!(exporter.io.written_settings.len(), 1);
        assert!(exporter.io.deleted.is_empty());
    }

    #[test]
    fn illegal_node_names_collect_the_verbatim_warning() {
        let mut scene = plain_node("Scene", 1);
        scene.children.push(plain_node(" Bad", 1));
        scene.children.push(plain_node("Worse ", 1));

        let input = ExportInput {
            filename: "/res/chars/names.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };
        let (exporter, _) = export(TestIo::new(), &input);
        assert_eq!(exporter.warnings().len(), 2);
        assert_eq!(
            exporter.warnings()[0],
            "Node \" Bad\" has an illegal name. Remove any spaces at the beginning and end of the name."
        );
        assert_eq!(
            exporter.warnings()[1],
            "Node \"Worse \" has an illegal name. Remove any spaces at the beginning and end of the name."
        );
    }

    #[test]
    fn duplicate_names_abort_with_sorted_messages() {
        let mut scene = plain_node("Scene", 1);
        scene.children = vec![
            plain_node("Dup", 1),
            plain_node("Dup", 1),
            plain_node("Also", 1),
            plain_node("Also", 1),
        ];

        let input = ExportInput {
            filename: "/res/chars/dup.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };
        let mut exporter = MfxExporter::new(TestIo::new());
        let error = exporter.do_export(&input).expect_err("duplicates abort");
        assert_eq!(
            error,
            ExportError::InvalidIds(
                "Duplicate node name Also\nDuplicate node name Dup\n".to_string()
            )
        );
        assert!(exporter.io.written_animations.is_empty());
    }

    #[test]
    fn a_path_outside_the_res_roots_aborts_with_the_verbatim_message() {
        let io = TestIo {
            res_root: String::new(), // dissolveFilename is the identity
            ..TestIo::new()
        };
        let input = ExportInput {
            filename: "C:/scratch/anim.animation".to_string(),
            scene: plain_node("Scene", 1),
            anim_range: (0, 0),
            ..Default::default()
        };
        let mut exporter = MfxExporter::new(io);
        let error = exporter
            .do_export(&input)
            .expect_err("outside the res path");
        assert_eq!(
            error.to_string(),
            "The exported file \"C:/scratch/anim.animation\" is not in a valid game path.\n\
             Need to export to a *subfolder* of:\n\
             C:/game/res"
        );
    }

    #[test]
    fn a_dissolved_name_without_a_subfolder_aborts_too() {
        // dissolveFilename shortens the path but the result has no
        // separator: still an abort.
        let mut exporter = MfxExporter::new(TestIo::new());
        let input = ExportInput {
            filename: "/res/bare.animation".to_string(),
            scene: plain_node("Scene", 1),
            anim_range: (0, 0),
            ..Default::default()
        };
        // "bare.animation" has no separator.
        assert!(matches!(
            exporter.do_export(&input),
            Err(ExportError::NotInResPath(_))
        ));
    }

    #[test]
    fn the_animation_name_crops_the_basename_and_the_extension() {
        // Leading-separator names are not cropped at the separator (the
        // reference's `> 0` check), so the dot crop acts on the whole
        // path. The fake host dissolves "/leading.animation" into a
        // subfolder so the res-path check passes.
        let input = ExportInput {
            filename: "/leading.animation".to_string(),
            scene: plain_node("Scene", 1),
            anim_range: (0, 0),
            ..Default::default()
        };
        let mut exporter = MfxExporter::new(TestIo {
            subfolder: true,
            ..TestIo::new()
        });
        exporter.do_export(&input).expect("export succeeds");
        let bytes = exporter
            .io
            .written_animations
            .values()
            .next()
            .cloned()
            .expect("an animation was written");
        let mut r = TestReader::new(&bytes);
        let _ = r.f32();
        assert_eq!(r.string(), "/leading");
        assert_eq!(r.string(), "/leading");
    }

    #[test]
    fn unit_scale_multiplies_the_position_keys() {
        let hand = translated_node("Hand", 1.0, 2.0, 3.0);
        let mut scene = plain_node("Scene", 1);
        scene.children.push(hand);

        let input = ExportInput {
            filename: "/res/chars/demo.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };
        let io = TestIo {
            unit_scale: 0.5,
            ..TestIo::new()
        };
        let (_, bytes) = export(io, &input);

        let (_, _, _, _, channels) = read_animation(&bytes);
        assert_eq!(channels.len(), 2);
        // Scene Root (all zero) then Hand.
        assert_eq!(channels[1].1, "Hand");
        assert_eq!(channels[1].3[0].1, Vector3::new(0.5, 1.0, 1.5));
    }

    #[test]
    fn allow_scale_keeps_the_raw_scale_and_default_normalises_it() {
        // A child bone whose world is scaled 2x in every axis. The root
        // channel is the stored identity (setMaxNode(NULL)), so the scale
        // only shows on the bone channel.
        let scaled_world = {
            let mut m = Matrix::identity();
            for (row, row_vals) in m.m.iter_mut().enumerate().take(3) {
                row_vals[row] = 2.0;
            }
            m.m[3][0] = 1.0;
            m
        };
        let mut scene = plain_node("Scene", 1);
        scene.children.push(SceneNode {
            name: "Bone".to_string(),
            world: vec![scaled_world],
            ..Default::default()
        });

        let input = ExportInput {
            filename: "/res/chars/demo.animation".to_string(),
            scene: scene.clone(),
            anim_range: (0, 0),
            ..Default::default()
        };

        // Default: allowScale false -> the world rows are normalised, the
        // scale key reads 1.
        let (_, bytes) = export(TestIo::new(), &input);
        let (_, _, _, _, channels) = read_animation(&bytes);
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[1].1, "Bone");
        assert_eq!(channels[1].2[0].1, Vector3::new(1.0, 1.0, 1.0));

        // allowScale true: the raw 2x reaches the scale keys.
        let file = settings_section(Some(true), None);
        let io = TestIo::new().with_section("/res/chars/demo.animationsettings", file);
        let (_, bytes) = export(io, &input);
        let (_, _, _, _, channels) = read_animation(&bytes);
        assert_eq!(channels[1].2[0].1, Vector3::new(2.0, 2.0, 2.0));
    }

    #[test]
    fn legacy_orientation_changes_the_rotation_keys() {
        // A 90-degree x rotation: rows (1,0,0)-(0,0,1)-(0,-1,0). The XOR
        // sign pattern lands on the off-diagonal cells here, unlike a z
        // rotation whose nonzero cells all sit in XOR-unchanged positions.
        let rotated = {
            let mut m = Matrix::identity();
            m.m[1] = [0.0, 0.0, 1.0, 0.0];
            m.m[2] = [0.0, -1.0, 0.0, 0.0];
            m
        };
        let mut scene = plain_node("Scene", 1);
        scene.children.push(SceneNode {
            name: "Bone".to_string(),
            world: vec![rotated],
            ..Default::default()
        });

        let input = ExportInput {
            filename: "/res/chars/demo.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };

        // Without the legacy orientation the rearranged rows keep
        // r1=(0,0,-1), r2=(0,1,0): x = -0.70711.
        let (_, bytes) = export(TestIo::new(), &input);
        let (_, _, _, _, channels) = read_animation(&bytes);
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[1].1, "Bone");
        let expected = Quaternion::new(
            -std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
        );
        assert!(
            channels[1].4[0].1.almost_equal(&expected),
            "{:?}",
            channels[1].4
        );

        // With it: x = +0.70711.
        let mut over = DataSection::new("settings");
        over.write_bool("useLegacyOrientation", true);
        let input = ExportInput {
            override_section: Some(over),
            ..input.clone()
        };
        let (_, bytes) = export(TestIo::new(), &input);
        let (_, _, _, _, channels) = read_animation(&bytes);
        assert_eq!(channels[1].1, "Bone");
        let expected = Quaternion::new(
            std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
        );
        assert!(
            channels[1].4[0].1.almost_equal(&expected),
            "{:?}",
            channels[1].4
        );
    }

    #[test]
    fn a_two_bone_walk_round_trips_key_times_and_values() {
        // Two frames: the child bone rotates 90 degrees about z between
        // them while sitting at a fixed offset under the hub.
        let rotation_z = |angle: f32| {
            let mut m = Matrix::identity();
            m.m[0][0] = angle.cos();
            m.m[0][1] = angle.sin();
            m.m[1][0] = -angle.sin();
            m.m[1][1] = angle.cos();
            m
        };
        let mut bone_world = Vec::new();
        for angle in [0.0f32, std::f32::consts::FRAC_PI_2] {
            let mut m = rotation_z(angle);
            m.m[3][0] = 2.0;
            bone_world.push(m);
        }
        let bone = SceneNode {
            name: "Bone".to_string(),
            world: bone_world,
            ..Default::default()
        };
        let mut hub = translated_node("Hub", 0.0, 1.0, 0.0);
        hub.world = vec![hub.world[0], hub.world[0]];
        hub.children.push(bone);
        let mut scene = plain_node("Scene", 2);
        scene.children.push(hub);

        let input = ExportInput {
            filename: "/res/chars/twobone.animation".to_string(),
            scene,
            anim_range: (0, 1),
            ..Default::default()
        };
        let (_, bytes) = export(TestIo::new(), &input);

        let mut r = TestReader::new(&bytes);
        assert_eq!(r.f32(), 1.0);
        assert_eq!(r.string(), "twobone");
        assert_eq!(r.string(), "twobone");
        assert_eq!(r.i32(), 3);

        // Channel 0: Scene Root (identity), consumed wholesale.
        assert_eq!(r.i32(), 1);
        assert_eq!(r.string(), "Scene Root");
        let _ = (r.vec3_seq(), r.vec3_seq(), r.quat_seq());
        let _ = (r.i32_seq(), r.i32_seq(), r.i32_seq());

        // Channel 1: Hub, relative to the root. Translation (0,1,0) times
        // the unit scale, identity rotation, times 0 and 1.
        assert_eq!(r.i32(), 1);
        assert_eq!(r.string(), "Hub");
        let scales = r.vec3_seq();
        let positions = r.vec3_seq();
        let rotations = r.quat_seq();
        let bound1 = r.i32_seq();
        let bound2 = r.i32_seq();
        let bound3 = r.i32_seq();
        assert_eq!(scales.len(), 2);
        assert_eq!(
            scales.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
            [0.0, 1.0]
        );
        assert_eq!(positions[0].1, Vector3::new(0.0, 0.01, 0.0));
        assert_eq!(positions[1].1, Vector3::new(0.0, 0.01, 0.0));
        assert_eq!(rotations[0].1, Quaternion::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(bound1, [1, 2]);
        assert_eq!(bound2, [1, 2]);
        assert_eq!(bound3, [1, 2]);

        // Channel 2: Bone, relative to Hub: frame 0 identity at (2,-1,0),
        // frame 1 the 90-degree rotation at the same offset.
        assert_eq!(r.i32(), 1);
        assert_eq!(r.string(), "Bone");
        let _ = r.vec3_seq();
        let positions = r.vec3_seq();
        let rotations = r.quat_seq();
        let bound = r.i32_seq();
        assert_eq!(positions[0].1, Vector3::new(0.02, -0.01, 0.0));
        assert_eq!(positions[1].1, Vector3::new(0.02, -0.01, 0.0));
        assert_eq!(rotations[0].1, Quaternion::new(0.0, 0.0, 0.0, 1.0));
        // The rearrange maps the Max z-rotation onto a -90-degree
        // y-rotation in BigWorld space.
        let expected = Quaternion::new(
            0.0,
            -std::f32::consts::FRAC_1_SQRT_2,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
        );
        assert!(rotations[1].1.almost_equal(&expected), "{rotations:?}");
        assert_eq!(bound, [1, 2]);
    }

    // --- coverage closes: the direct tree faces and the name quirks ---

    /// A three-node chain: Scene -> Arm -> Hand (indices 0, 1, 2).
    fn chain_tree() -> MfxTree {
        let settings = ExportSettings::default();
        let mut exporter = MfxExporter::new(TestIo::new());
        let hand = plain_node("Hand", 1);
        let mut arm = plain_node("Arm", 1);
        arm.children.push(hand);
        let mut scene = plain_node("Scene", 1);
        scene.children.push(arm);
        exporter.pre_process(&settings, &scene)
    }

    #[test]
    fn find_of_an_absent_identifier_answers_none() {
        let tree = chain_tree();
        assert_eq!(tree.find("Ghost"), None);
    }

    #[test]
    fn reparent_is_a_no_op_when_the_parent_is_already_right() {
        let mut tree = chain_tree();
        // Hand's parent is already Arm.
        tree.reparent(2, 1);
        assert_eq!(tree.nodes[2].parent, Some(1));
        assert_eq!(tree.nodes[1].children, vec![2]);
    }

    #[test]
    fn reparent_moves_the_subtree_hanging_off_c_back_first() {
        let mut tree = chain_tree();
        // Arm under Hand: Hand hangs off Arm, so Hand moves back to Arm's
        // parent (the root) first, then Arm goes under Hand.
        tree.reparent(1, 2);
        assert_eq!(tree.nodes[2].parent, Some(0));
        assert_eq!(tree.nodes[1].parent, Some(2));
        assert_eq!(tree.nodes[0].children, vec![2]);
        assert_eq!(tree.nodes[2].children, vec![1]);
    }

    #[test]
    fn reparent_of_the_root_attaches_without_detaching() {
        let mut tree = chain_tree();
        // The root has no parent, so nothing detaches on either side (the
        // reference would dereference NULL; the port stays defined).
        tree.reparent(tree.root, 1);
        assert_eq!(tree.nodes[tree.root].parent, Some(1));
        assert!(tree.nodes[1].children.contains(&tree.root));
    }

    #[test]
    fn a_singular_ideal_parent_inverts_to_the_zero_matrix() {
        let settings = ExportSettings::default();
        let mut exporter = MfxExporter::new(TestIo::new());
        let mut bone = plain_node("Bone", 1);
        bone.world = vec![Matrix::identity()];
        let mut scene = plain_node("Scene", 1);
        scene.world = vec![Matrix::zero()];
        scene.children.push(bone);
        let tree = exporter.pre_process(&settings, &scene);

        // With no ideal parent passed, the node's own parent is the ideal.
        let rel = tree.get_relative_transform(1, 0, false, None);
        assert_eq!(rel, Matrix::zero());
    }

    #[test]
    fn a_node_without_a_parent_returns_its_own_world() {
        let mut tree = chain_tree();
        tree.nodes[tree.root].scene = Some(0);
        let rel = tree.get_relative_transform(tree.root, 0, false, None);
        assert_eq!(rel, Matrix::identity());
    }

    #[test]
    fn a_stored_node_with_a_parent_multiplies_up_the_chain() {
        let mut tree = chain_tree();
        // Hand keeps its tree parent but loses the max node: the stored
        // transform composes with the parent's world.
        tree.nodes[2].scene = None;
        tree.nodes[2].transform = Matrix::identity();
        assert_eq!(tree.get_transform(2, 0, false), Matrix::identity());
        assert_eq!(tree.get_transform(2, 0, true), Matrix::identity());
    }

    #[test]
    fn normalise_keeps_zero_rows_zero() {
        let mut m = Matrix::zero();
        m.m[0] = [2.0, 0.0, 0.0, 0.0];
        let out = normalise_matrix(&m);
        assert_eq!(out.m[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(out.m[1], [0.0, 0.0, 0.0, 0.0]);
        assert_eq!(out.m[2], [0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn write_settings_without_a_section_answers_false() {
        assert!(!ExportSettings::default().write_settings(None));
    }

    #[test]
    fn an_empty_settings_section_keeps_the_defaults() {
        let mut settings = ExportSettings::default();
        assert!(settings.read_settings(Some(&settings_section(None, None))));
        assert_eq!(settings, ExportSettings::default());

        // The single-key section only flips its own key.
        assert!(settings.read_settings(Some(&settings_section(None, Some(false)))));
        assert!(!settings.export_node_animation);
        assert_eq!(settings.allow_scale, ExportSettings::default().allow_scale);
    }

    #[test]
    fn harvest_tolerates_negative_and_truncated_strings() {
        // f32 total, a negative-length identifier, an empty one, one
        // channel of type 4, then an identifier length that outruns the
        // buffer (the errors behind it read as zeros).
        let mut data = Vec::new();
        w_f32(&mut data, 1.0);
        w_i32(&mut data, -3);
        w_i32(&mut data, 0);
        w_i32(&mut data, 1);
        w_i32(&mut data, 4);
        w_i32(&mut data, 5);
        data.extend_from_slice(b"ab");

        let ci = harvest_compression_info(&data);
        assert!(ci.specify_amounts);
        assert_eq!(ci.scale_compression_error, 0.0);
        assert_eq!(ci.position_compression_error, 0.0);
        assert_eq!(ci.rotation_compression_error, 0.0);
    }

    #[test]
    fn a_cut_inside_a_utf8_sequence_strips_nothing() {
        // The ten-character cut lands inside the last character, so the
        // whole name is kept and ".animation" is appended.
        let input = ExportInput {
            filename: "/res/chars/x好abcdefgh".to_string(),
            scene: plain_node("Scene", 1),
            anim_range: (0, 0),
            ..Default::default()
        };
        let (exporter, bytes) = export(TestIo::new(), &input);
        let written: Vec<&str> = exporter
            .io
            .written_animations
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(written, ["/res/chars/x好abcdefgh.animation"]);
        let mut r = TestReader::new(&bytes);
        let _ = r.f32();
        assert_eq!(r.string(), "x好abcdefgh");
        assert_eq!(r.string(), "x好abcdefgh");
    }

    #[test]
    fn a_leading_dot_basename_keeps_the_dot() {
        // "chars//.animation": the separator crop lands past the double
        // slash, so the basename is ".animation" and the dot crop sees
        // i == 0 — the reference's `> 0` keeps the whole name.
        let input = ExportInput {
            filename: "/res/chars//.animation".to_string(),
            scene: plain_node("Scene", 1),
            anim_range: (0, 0),
            ..Default::default()
        };
        let (_, bytes) = export(TestIo::new(), &input);
        let mut r = TestReader::new(&bytes);
        let _ = r.f32();
        assert_eq!(r.string(), ".animation");
        assert_eq!(r.string(), ".animation");
    }

    #[test]
    fn duplicate_reference_pairs_take_the_last_parent_and_unknown_names_skip() {
        let hand = translated_node("Hand", 1.0, 0.0, 0.0);
        let mut arm = plain_node("Arm", 1);
        arm.children.push(hand);
        let mut scene = plain_node("Scene", 1);
        scene.children.push(arm);

        // Document order: Arm (under Scene) with Hand inside, then Hand
        // again directly under Scene (the duplicate — the last pair wins),
        // then a pair naming an absent node (skipped).
        let mut visual = DataSection::new("visual");
        let root_node = visual.add_child("node");
        root_node.write_string("identifier", "Scene");
        let arm_node = root_node.add_child("node");
        arm_node.write_string("identifier", "Arm");
        let nested = arm_node.add_child("node");
        nested.write_string("identifier", "Hand");
        let direct = root_node.add_child("node");
        direct.write_string("identifier", "Hand");
        let ghost = root_node.add_child("node");
        ghost.write_string("identifier", "Ghost");

        let mut file = DataSection::new("root");
        file.write_string("referenceNodesFile", "chars/dup.visual");
        let io = TestIo::new()
            .with_section("chars/dup.visual", visual)
            .with_section("/res/chars/dup.animationsettings", file);
        let input = ExportInput {
            filename: "/res/chars/dup.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };
        let (_, bytes) = export(io, &input);
        let (_, _, _, _, channels) = read_animation(&bytes);
        // Hand re-parented under the root, Arm untouched, Ghost gone.
        assert_eq!(
            channels.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(),
            ["Scene Root", "Arm", "Hand"]
        );
        assert_eq!(channels[2].3[0].1, Vector3::new(0.01, 0.0, 0.0));
    }

    #[test]
    fn a_reference_visual_without_a_root_node_section_is_ignored() {
        let hand = translated_node("Hand", 8.0, 4.0, 0.5);
        let mut arm = translated_node("Arm", 4.0, 0.0, 0.0);
        arm.children.push(hand);
        let mut scene = plain_node("Scene", 1);
        scene.children.push(arm);

        // No "node" child: the re-parent is skipped (the reference
        // dereferences NULL there).
        let visual = DataSection::new("visual");
        let mut file = DataSection::new("root");
        file.write_string("referenceNodesFile", "chars/empty.visual");
        let io = TestIo::new()
            .with_section("chars/empty.visual", visual)
            .with_section("/res/chars/keep.animationsettings", file);
        let input = ExportInput {
            filename: "/res/chars/keep.animation".to_string(),
            scene,
            anim_range: (0, 0),
            ..Default::default()
        };
        let (_, bytes) = export(io, &input);
        let (_, _, _, _, channels) = read_animation(&bytes);
        // No re-parent: Hand stays relative to Arm.
        assert_eq!(channels[2].1, "Hand");
        assert_eq!(channels[2].3[0].1, Vector3::new(0.04, 0.04, 0.005));
    }
}
