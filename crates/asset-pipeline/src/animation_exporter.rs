//! BW-13-05-02 the Max animation exporter's portable core
//! (`tools/animationexporter`).
//!
//! The reference is the 3ds Max `animationexporter` plugin: its `DoExport`
//! walks the scene node tree, samples each included node's
//! parent-relative transform per frame, decomposes the samples into
//! scale/translation/rotation (`lib/math/blend_transform.cpp`), and writes
//! the `.animation` binary the engine loads (`lib/moo/animation.cpp`).
//! That binary is the whole contract: the reader face already lives in
//! `client`'s skeletal port, and this module is its writer — the same
//! file format, the same channel grammar (type 1 plain node channel, type
//! 4 with compression amounts, type 6 cue track), the same keyframe
//! sequences.
//!
//! What is portable is carried: the node tree with its include flags and
//! the ancestor-propagation and root-sorting passes (`mungeIncluded`,
//! the descending child-count re-order of the root's immediate children),
//! the per-frame sampling loop with its relative-transform, axis-rearrange
//! and decompose steps (`mfxnode.cpp` `MFXNode::exportAnimation`,
//! `utility.cpp` `rearrangeMatrix`/`normaliseMatrix`), the compression
//! amounts an existing file contributes (the header peek `DoExport` does
//! before overwriting it), the cue track's note-track grammar
//! (`cuetack.cpp`) and the file-name cropping. What is not, stays out:
//! the Max SDK scene walk (`INode::GetNodeTM`, modifiers, note-track
//! enumeration, dialogs) becomes the caller's sampler closure — one
//! world-space transform per (identifier, frame) — and the plugin's file
//! and message-box faces become the returned bytes.
//!
//! The axis rearrange carries the input's translation row through: the
//! shipped fixtures show it survives — `crab_walkforward.animation`'s
//! `CrabHub` position keys equal `crab.visual`'s `CrabHub` node
//! translation row. The layout of the wire is likewise fixture-pinned:
//! little-endian scalars, length-prefixed strings, `u32`-counted
//! sequences, keys stored as `(time, components)` pairs and the three
//! trailing bound tables the reader rebuilds and drops.

use std::collections::BTreeMap;

use foundation::math::Matrix;
use foundation::math::Quaternion;
use foundation::math::Vector3;

/// One node of the exporter's scene tree (`MFXNode`, `mfxnode.hpp`): the
/// identifier the channel will carry, the include flag, and the children
/// in scene order. The Max node itself is the caller's business — a node
/// the sampler answers for is a scene node, one it does not is a
/// hierarchy-only node (the reference's `node_ == NULL` case, whose
/// stored transform is the identity and whose world is its parent's).
#[derive(Debug, Clone, PartialEq)]
pub struct ExportNode {
    identifier: String,
    include: bool,
    children: Vec<ExportNode>,
}

impl ExportNode {
    /// The default `MFXNode`: identity transform, not included.
    pub fn new(identifier: &str) -> Self {
        Self {
            identifier: identifier.to_string(),
            include: false,
            children: Vec::new(),
        }
    }

    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    pub fn set_identifier(&mut self, identifier: &str) {
        self.identifier = identifier.to_string();
    }

    pub fn include(&self) -> bool {
        self.include
    }

    pub fn set_include(&mut self, include: bool) {
        self.include = include;
    }

    pub fn add_child(&mut self, child: ExportNode) {
        self.children.push(child);
    }

    pub fn n_children(&self) -> usize {
        self.children.len()
    }

    pub fn children(&self) -> &[ExportNode] {
        &self.children
    }

    pub fn children_mut(&mut self) -> &mut [ExportNode] {
        &mut self.children
    }

    /// `MFXNode::treeSize` (`mfxnode.cpp:133-145`): this node plus every
    /// descendant.
    pub fn tree_size(&self) -> usize {
        1 + self
            .children
            .iter()
            .map(ExportNode::tree_size)
            .sum::<usize>()
    }

    /// `MFXNode::nIncludedNodes` (`mfxnode.cpp:250-262`): this node if
    /// included plus every included descendant — the node channels the
    /// export writes.
    pub fn n_included_nodes(&self) -> usize {
        let included = usize::from(self.include);
        included
            + self
                .children
                .iter()
                .map(ExportNode::n_included_nodes)
                .sum::<usize>()
    }

    /// `mungeIncluded` (`mfxexp.cpp:220-230`), driving `includeAncestors`
    /// (`mfxnode.cpp:101-110`): every ancestor of an included node is
    /// included, so every channel's parent-relative transform hangs from
    /// an included parent. Returns whether the subtree contains an
    /// included node.
    pub fn munge_included(&mut self) -> bool {
        let mut any = self.include;
        for child in &mut self.children {
            any |= child.munge_included();
        }
        if any {
            self.include = true;
        }
        any
    }

    /// `MFXNode::find` — depth-first by identifier.
    pub fn find(&self, identifier: &str) -> Option<&ExportNode> {
        if self.identifier == identifier {
            return Some(self);
        }
        self.children
            .iter()
            .find_map(|child| child.find(identifier))
    }
}

/// The exporter settings the portable core reads
/// (`expsets.hpp`/`expsets.cpp`). The frame range defaults to `0..=100`
/// (`expsets.cpp:23-24`); `unitScale` is the reference's
/// `GetMasterScale(UNITS_CENTIMETERS) / 100` — the Max scene unit the
/// caller converts with, 1.0 when the world is already in metres.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSettings {
    pub first_frame: i32,
    pub last_frame: i32,
    /// `allowScale`: false normalises every world transform before the
    /// relative step, so no scale survives into the keys.
    pub allow_scale: bool,
    /// `exportNodeAnimation`: false writes no node channels at all.
    pub export_node_animation: bool,
    /// `exportCueTrack`: the cue channel is appended when a cue track with
    /// cues is handed over.
    pub export_cue_track: bool,
    /// `useLegacyOrientation`: the rearrange's extra negations.
    pub use_legacy_orientation: bool,
    pub unit_scale: f32,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            first_frame: 0,
            last_frame: 100,
            allow_scale: false,
            export_node_animation: true,
            export_cue_track: false,
            use_legacy_orientation: false,
            unit_scale: 1.0,
        }
    }
}

impl ExportSettings {
    /// `ExportSettings::setFrameRange` (`expsets.hpp:44`).
    pub fn set_frame_range(&mut self, first: i32, last: i32) {
        self.first_frame = first;
        self.last_frame = last;
    }
}

/// The compression amounts a type-4 channel carries
/// (`MFXExport::DoExport`'s `CompressionInfo`, `mfxexp.hpp`). The
/// reference's fields are uninitialised until a previous file supplies
/// them; the port zeroes them, and they are written only when
/// `specify_amounts` is set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompressionInfo {
    pub specify_amounts: bool,
    pub scale_compression_error: f32,
    pub position_compression_error: f32,
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

/// `DoExport`'s header peek (`mfxexp.cpp:439-467`): an existing
/// `.animation`'s first channel, when it is a type-4 node channel,
/// donates its compression amounts to the export that overwrites the
/// file. The reference ignores every read error — a missing, truncated or
/// differently-shaped file leaves the amounts unspecified (the type-1
/// channel grammar).
pub fn read_compression_info(data: &[u8]) -> CompressionInfo {
    let mut r = Reader::new(data);
    let _total_time = r.f32();
    let _identifier = r.string();
    let _internal_identifier = r.string();
    let num_channels = r.i32();
    if !r.ok() || num_channels <= 0 {
        return CompressionInfo::default();
    }
    let channel_type = r.i32();
    if !r.ok() || channel_type != 4 {
        return CompressionInfo::default();
    }
    let _identifier = r.string();
    let mut ci = CompressionInfo {
        specify_amounts: true,
        ..CompressionInfo::default()
    };
    ci.scale_compression_error = r.f32();
    ci.position_compression_error = r.f32();
    ci.rotation_compression_error = r.f32();
    if !r.ok() {
        return CompressionInfo::default();
    }
    ci
}

/// The note-track cue track (`cuetrack.cpp`). A note is `cue:name` or
/// `sound:name`; the cue strings become `c`/`s` prefixed, keyed by frame
/// offset from the export's first frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CueTrack {
    /// Cues by frame offset from the first frame, insertion-ordered per
    /// frame: `std::map<TimeValue, std::list<BW::string>>` keyed by the
    /// note key's tick time minus `firstFrame * GetTicksPerFrame()`.
    cues: BTreeMap<i32, Vec<String>>,
}

impl CueTrack {
    /// `CueTrack::addCue` (`cuetrack.cpp:14-51`). `frame` is the note key's
    /// time in frames — the reference divides the tick time by
    /// `GetTicksPerFrame()` here and subtracts the first frame's ticks,
    /// and `writeFile` divides the ticks back out, so the ticks cancel and
    /// only the frame offset remains. A cue outside the frame range is
    /// dropped, a note without a `:` or with an unknown prefix is
    /// dropped, and a repeated cue at the same frame is dropped.
    pub fn add_cue(&mut self, frame: i32, note: &str, first_frame: i32, last_frame: i32) {
        if frame < first_frame || frame > last_frame {
            return;
        }
        let time = frame - first_frame;

        let split = match note.find(':') {
            Some(at) => at,
            None => return,
        };
        let (kind, rest) = note.split_at(split);
        let rest = &rest[1..];
        let cue = match kind {
            "sound" => format!("s{rest}"),
            "cue" => format!("c{rest}"),
            _ => return,
        };

        let entry = self.cues.entry(time).or_default();
        if entry.contains(&cue) {
            return;
        }
        entry.push(cue);
    }

    /// `CueTrack::hasCues` (`cuetrack.cpp:86-100`).
    pub fn has_cues(&self) -> bool {
        self.cues.values().any(|entry| !entry.is_empty())
    }

    /// `CueTrack::clear` (`cuetrack.cpp:9-12`).
    pub fn clear(&mut self) {
        self.cues.clear();
    }

    /// The cues the channel would carry, by frame offset.
    pub fn cues(&self) -> impl Iterator<Item = (i32, &[String])> {
        self.cues
            .iter()
            .map(|(time, entry)| (*time, entry.as_slice()))
    }

    /// `CueTrack::writeFile` (`cuetrack.cpp:54-84`): nothing at all when
    /// there are no cues — not even the channel type.
    fn write(&self, w: &mut Writer) {
        let num: usize = self.cues.values().map(Vec::len).sum();
        if num == 0 {
            return;
        }
        w.i32(6);
        w.i32(num as i32);
        for (time, entry) in &self.cues {
            for cue in entry {
                w.f32(*time as f32);
                w.string(cue);
                // no additional args
                w.i32(0);
            }
        }
    }
}

/// `DoExport`'s name crop (`mfxexp.cpp:424-436`): everything past the last
/// path separator, then past the last dot. A separator at index 0 or a dot
/// at index 0 leaves the name whole (the reference's `> 0` guards).
pub fn animation_name(path: &str) -> &str {
    let mut name = path;
    if let Some(at) = name.rfind(['/', '\\'])
        && at > 0
    {
        name = &name[at + 1..];
    }
    if let Some(at) = name.rfind('.')
        && at > 0
    {
        name = &name[..at];
    }
    name
}

/// `trailingLeadingWhitespaces` (`utility.cpp:88-94`): the exporter warns
/// about a node name with a leading or trailing space (the reference pops
/// a message box and carries on; the port carries the predicate).
pub fn has_trailing_leading_whitespace(name: &str) -> bool {
    name.starts_with(' ') || name.ends_with(' ')
}

/// The export (`MFXExport::DoExport`, `mfxexp.cpp:224-511` — its write
/// face): the include pass, the root pass and the channel writes, minus
/// the file, the settings dialogs and the message boxes.
///
/// The root takes the fixed `Scene Root` identifier and is detached from
/// the sampler (the reference's `setMaxNode(NULL)`), so its channel is the
/// identity pose — the fixture-verified `Scene Root` channel every
/// shipped `.animation` opens with. `sample` answers each node's
/// world-space transform at a frame; `None` marks a hierarchy-only node.
/// The header carries the animation's own name twice (identifier and
/// internal identifier), the total time as `lastFrame - firstFrame` and
/// the channel count; an export with no channels at all writes no bytes
/// (the reference writes no file).
pub fn export(
    root: &mut ExportNode,
    sample: impl Fn(&str, i32) -> Option<Matrix>,
    settings: &ExportSettings,
    cue_track: Option<&CueTrack>,
    compression: CompressionInfo,
    anim_name: &str,
) -> Vec<u8> {
    root.set_identifier("Scene Root");
    root.munge_included();
    sort_root_children(root);

    let mut n_channels = 0;
    if settings.export_node_animation {
        n_channels += root.n_included_nodes();
    }
    if settings.export_cue_track && cue_track.is_some_and(CueTrack::has_cues) {
        n_channels += 1;
    }
    if n_channels == 0 {
        return Vec::new();
    }

    let mut w = Writer::default();
    w.f32((settings.last_frame - settings.first_frame) as f32);
    w.string(anim_name);
    w.string(anim_name);
    w.i32(n_channels as i32);

    if settings.export_node_animation {
        export_node_animation(&mut w, root, &[], None, settings, compression, &sample);
    }
    if settings.export_cue_track
        && let Some(cue_track) = cue_track
    {
        cue_track.write(&mut w);
    }
    w.bytes
}

/// `MFXNode::exportAnimation` (`mfxnode.cpp:170-262`), one recursion step:
/// the node's own channel when included, then the children in scene
/// order. `ancestors` is the node path from the root down to this node's
/// parent (`ancestors[0]` is the root); `ideal` is the nearest included
/// ancestor — the parent the channel's keys are relative to.
fn export_node_animation(
    w: &mut Writer,
    node: &ExportNode,
    ancestors: &[&ExportNode],
    ideal: Option<&ExportNode>,
    settings: &ExportSettings,
    compression: CompressionInfo,
    sample: &impl Fn(&str, i32) -> Option<Matrix>,
) {
    if node.include {
        // `getRelativeTransform` (`mfxnode.cpp:70-120`): the ideal
        // parent, defaulting to the immediate one; the node's world
        // against its inverse, or the world alone at the top.
        let ideal_parent = ideal.or_else(|| ancestors.last().copied());
        let mut scales: Vec<(f32, Vector3)> = Vec::new();
        let mut positions: Vec<(f32, Vector3)> = Vec::new();
        let mut rotations: Vec<(f32, Quaternion)> = Vec::new();
        let mut bound_table: Vec<i32> = Vec::new();

        let mut time = 0.0f32;
        for frame in settings.first_frame..=settings.last_frame {
            // `getRelativeTransform` (`mfxnode.cpp:70-120`): a sampled
            // node's world against the ideal parent's inverse — or its
            // own world at the top — while a hierarchy-only node's
            // relative transform is its stored identity, whatever its
            // parent's world (`transform_`).
            let own = own_world(node, ancestors, frame, settings, sample);
            let relative = match own {
                Some(world) => match ideal_parent {
                    Some(parent) => {
                        let parent_world =
                            ancestor_world(parent, ancestors, frame, settings, sample);
                        let mut inverse = parent_world;
                        invert_affine(&mut inverse);
                        let mut relative = Matrix::zero();
                        relative.multiply(&world, &inverse);
                        relative
                    }
                    None => world,
                },
                None => Matrix::identity(),
            };

            // `mfxnode.cpp:231-263`: rearrange, decompose, normalise the
            // rotation, scale the translation.
            let rearranged = rearrange_matrix(&relative, settings.use_legacy_orientation);
            let (mut rotation, scale, translation) = decompose(&rearranged);
            rotation.normalise();
            bound_table.push(frame - settings.first_frame + 1);
            scales.push((time, scale));
            positions.push((
                time,
                Vector3::new(
                    translation.x * settings.unit_scale,
                    translation.y * settings.unit_scale,
                    translation.z * settings.unit_scale,
                ),
            ));
            rotations.push((time, rotation));
            time += 1.0;
        }

        // The channel head (`mfxnode.cpp:218-232`), then the key
        // sequences and the three bound tables (`mfxnode.cpp:233-237`).
        if compression.specify_amounts {
            w.i32(4);
            w.string(&node.identifier);
            w.f32(compression.scale_compression_error);
            w.f32(compression.position_compression_error);
            w.f32(compression.rotation_compression_error);
        } else {
            w.i32(1);
            w.string(&node.identifier);
        }
        w.f32_seq_vec3(&scales);
        w.f32_seq_vec3(&positions);
        w.f32_seq_quat(&rotations);
        w.i32_seq(&bound_table);
        w.i32_seq(&bound_table);
        w.i32_seq(&bound_table);
    }

    let next_ideal = if node.include { Some(node) } else { ideal };
    let mut path: Vec<&ExportNode> = ancestors.to_vec();
    path.push(node);
    for child in &node.children {
        export_node_animation(w, child, &path, next_ideal, settings, compression, sample);
    }
}

/// `MFXNode::getTransform`'s `node_` arm (`mfxnode.cpp:41-47`): the
/// node's own world transform, normalised to strip scale when the export
/// forbids it. The root answers `None` — the reference detaches it
/// before exporting (`setMaxNode(NULL)`), and an empty ancestor path is
/// how the port sees that.
fn own_world(
    node: &ExportNode,
    ancestors: &[&ExportNode],
    frame: i32,
    settings: &ExportSettings,
    sample: &impl Fn(&str, i32) -> Option<Matrix>,
) -> Option<Matrix> {
    if ancestors.is_empty() {
        return None;
    }
    sample(node.identifier(), frame).map(|world| {
        if !settings.allow_scale {
            normalise_matrix(&world)
        } else {
            world
        }
    })
}

/// A hierarchy-only node's world: its parent's, resolved up the chain —
/// the nearest sampled ancestor's transform (`getTransform`'s parent arm
/// over the identity `transform_`), and the identity at the top.
/// `ancestors[0]` is the root: detached, never sampled.
fn ancestor_chain_world(
    ancestors: &[&ExportNode],
    frame: i32,
    settings: &ExportSettings,
    sample: &impl Fn(&str, i32) -> Option<Matrix>,
) -> Matrix {
    for (index, node_up) in ancestors.iter().enumerate().rev() {
        if index == 0 {
            break;
        }
        if let Some(world) = own_world(node_up, &ancestors[..index], frame, settings, sample) {
            return world;
        }
    }
    Matrix::identity()
}

/// `MFXNode::getTransform` (`mfxnode.cpp:36-66`): the node's own world
/// transform when it is sampled, otherwise the chain's.
fn world_of(
    node: &ExportNode,
    ancestors: &[&ExportNode],
    frame: i32,
    settings: &ExportSettings,
    sample: &impl Fn(&str, i32) -> Option<Matrix>,
) -> Matrix {
    own_world(node, ancestors, frame, settings, sample)
        .unwrap_or_else(|| ancestor_chain_world(ancestors, frame, settings, sample))
}

/// An ancestor's world transform: the same resolution over that
/// ancestor's own path (`ancestors` truncated to its position).
fn ancestor_world(
    ancestor: &ExportNode,
    ancestors: &[&ExportNode],
    frame: i32,
    settings: &ExportSettings,
    sample: &impl Fn(&str, i32) -> Option<Matrix>,
) -> Matrix {
    let index = ancestors
        .iter()
        .position(|node| std::ptr::eq(*node, ancestor))
        .expect("the ideal parent is an ancestor");
    world_of(ancestor, &ancestors[..index], frame, settings, sample)
}

/// `rearrangeMatrix` (`utility.cpp:42-64`): Max's Z-up to the engine's
/// Y-up — columns one and two trade places, basis rows one and two trade
/// places, and the legacy orientation adds the row/column negations in
/// the reference's order (rows first, then columns, so the shared cells
/// flip twice and stay). The translation row is carried from the input:
/// the fixtures show it survives (the `CrabHub` position keys equal the
/// visual node's translation row).
fn rearrange_matrix(m: &Matrix, legacy: bool) -> Matrix {
    let mut mm = Matrix::identity();
    for r in 0..3 {
        mm.m[r][0] = m.m[r][0];
        mm.m[r][1] = m.m[r][2];
        mm.m[r][2] = m.m[r][1];
    }
    for c in 0..3 {
        let row2 = mm.m[2][c];
        mm.m[2][c] = mm.m[1][c];
        mm.m[1][c] = row2;
    }
    if legacy {
        for c in 0..3 {
            mm.m[2][c] = -mm.m[2][c];
            mm.m[0][c] = -mm.m[0][c];
        }
        for row in mm.m.iter_mut().take(3) {
            row[2] = -row[2];
            row[0] = -row[0];
        }
    }
    mm.m[3][0] = m.m[3][0];
    mm.m[3][1] = m.m[3][1];
    mm.m[3][2] = m.m[3][2];
    mm.m[3][3] = 1.0;
    mm
}

/// `normaliseMatrix` (`utility.cpp:71-86`): the basis rows scaled to unit
/// length, the translation row copied.
fn normalise_matrix(m: &Matrix) -> Matrix {
    let mut out = *m;
    for r in 0..3 {
        let row = Vector3::new(m.m[r][0], m.m[r][1], m.m[r][2]);
        let length = row.length();
        if length != 0.0 {
            out.m[r][0] /= length;
            out.m[r][1] /= length;
            out.m[r][2] /= length;
        }
    }
    out
}

/// The affine inverse the relative transform needs (Max's `Inverse` on a
/// basis-rows-plus-translation matrix): the 3x3 basis inverted through
/// its adjugate, the translation row re-expressed in the inverted basis
/// (`w = v * M` maps to `v_local * R + T`, so the inverse row is
/// `-T * R⁻¹`). A singular basis has no reference-defined inverse; the
/// port answers the identity, which keeps a degenerate input producing
/// degenerate keys instead of undefined memory.
fn invert_affine(m: &mut Matrix) {
    let det = m.m[0][0] * (m.m[1][1] * m.m[2][2] - m.m[1][2] * m.m[2][1])
        - m.m[0][1] * (m.m[1][0] * m.m[2][2] - m.m[1][2] * m.m[2][0])
        + m.m[0][2] * (m.m[1][0] * m.m[2][1] - m.m[1][1] * m.m[2][0]);
    if det == 0.0 {
        *m = Matrix::identity();
        return;
    }
    let inv_det = 1.0 / det;
    let mut inv = Matrix::zero();
    for (i, row) in inv.m.iter_mut().enumerate().take(3) {
        for (j, cell) in row.iter_mut().enumerate().take(3) {
            // inverse[i][j] = cofactor(j, i) / det
            let (a, b) = ((j + 1) % 3, (j + 2) % 3);
            let (c, d) = ((i + 1) % 3, (i + 2) % 3);
            *cell = (m.m[a][c] * m.m[b][d] - m.m[a][d] * m.m[b][c]) * inv_det;
        }
    }
    let translation = Vector3::new(m.m[3][0], m.m[3][1], m.m[3][2]);
    inv.m[3][0] =
        -(translation.x * inv.m[0][0] + translation.y * inv.m[1][0] + translation.z * inv.m[2][0]);
    inv.m[3][1] =
        -(translation.x * inv.m[0][1] + translation.y * inv.m[1][1] + translation.z * inv.m[2][1]);
    inv.m[3][2] =
        -(translation.x * inv.m[0][2] + translation.y * inv.m[1][2] + translation.z * inv.m[2][2]);
    inv.m[3][3] = 1.0;
    *m = inv;
}

/// `BlendTransform::init` (`blend_transform.cpp:54-86`) with the
/// exporter's `normaliseRotation` (`mfxnode.cpp:257`): the scale is the
/// basis row lengths, the rows normalise to the rotation, a left-handed
/// basis flips the third row (and its scale), the translation is the
/// fourth row, and the rotation quaternion comes from the matrix.
fn decompose(m: &Matrix) -> (Quaternion, Vector3, Vector3) {
    let mut rows = [
        Vector3::new(m.m[0][0], m.m[0][1], m.m[0][2]),
        Vector3::new(m.m[1][0], m.m[1][1], m.m[1][2]),
        Vector3::new(m.m[2][0], m.m[2][1], m.m[2][2]),
    ];
    let mut scale = Vector3::new(rows[0].length(), rows[1].length(), rows[2].length());
    for (row, s) in rows.iter_mut().zip([scale.x, scale.y, scale.z]) {
        row.scale(1.0 / s);
    }

    // XPVec3Cross then XPVec3Dot (`vector3.ipp:122-133`).
    let cross = Vector3::new(
        rows[0].y * rows[1].z - rows[0].z * rows[1].y,
        rows[0].z * rows[1].x - rows[0].x * rows[1].z,
        rows[0].x * rows[1].y - rows[0].y * rows[1].x,
    );
    if cross.dot_product(&rows[2]) < 0.0 {
        rows[2].scale(-1.0);
        scale.z *= -1.0;
    }

    let translation = Vector3::new(m.m[3][0], m.m[3][1], m.m[3][2]);

    let mut rotation_matrix = Matrix::zero();
    for (r, row) in rows.iter().enumerate() {
        rotation_matrix.m[r][0] = row.x;
        rotation_matrix.m[r][1] = row.y;
        rotation_matrix.m[r][2] = row.z;
    }
    rotation_matrix.m[3][3] = 1.0;
    let mut rotation = Quaternion::from_matrix(&rotation_matrix);
    rotation.normalise();
    (rotation, scale, translation)
}

/// The root's immediate children re-ordered by descending direct-child
/// count (`mfxexp.cpp:383-399`, `numChildDescending`): `std::list::sort`
/// is stable, so equally-sized children keep their scene order.
fn sort_root_children(root: &mut ExportNode) {
    root.children
        .sort_by_key(|child| std::cmp::Reverse(child.n_children()));
}

/// The `BinaryFile` wire (`cstdmf/binaryfile.hpp`): little-endian
/// scalars, `int32`-length-prefixed strings, `writeSequence`'s `u32`
/// count then the elements — pairs laid out as their members in order,
/// `(time, components)`.
#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn f32(&mut self, value: f32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn string(&mut self, value: &str) {
        self.i32(value.len() as i32);
        self.bytes.extend_from_slice(value.as_bytes());
    }

    fn f32_seq_vec3(&mut self, keys: &[(f32, Vector3)]) {
        self.bytes
            .extend_from_slice(&(keys.len() as u32).to_le_bytes());
        for (time, v) in keys {
            self.f32(*time);
            self.f32(v.x);
            self.f32(v.y);
            self.f32(v.z);
        }
    }

    fn f32_seq_quat(&mut self, keys: &[(f32, Quaternion)]) {
        self.bytes
            .extend_from_slice(&(keys.len() as u32).to_le_bytes());
        for (time, q) in keys {
            self.f32(*time);
            self.f32(q.x);
            self.f32(q.y);
            self.f32(q.z);
            self.f32(q.w);
        }
    }

    fn i32_seq(&mut self, values: &[i32]) {
        self.bytes
            .extend_from_slice(&(values.len() as u32).to_le_bytes());
        for value in values {
            self.i32(*value);
        }
    }
}

/// The header peek's read side: little-endian scalars and
/// length-prefixed strings over the raw bytes, failing soft on
/// truncation the way `BinaryFile`'s error flag does.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    failed: bool,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            failed: false,
        }
    }

    fn ok(&self) -> bool {
        !self.failed
    }

    fn read(&mut self, len: usize) -> &'a [u8] {
        if self.failed || self.pos + len > self.data.len() {
            self.failed = true;
            return &[];
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        slice
    }

    fn f32(&mut self) -> f32 {
        let bytes = self.read(4);
        if bytes.len() < 4 {
            return 0.0;
        }
        let mut a = [0u8; 4];
        a.copy_from_slice(bytes);
        f32::from_le_bytes(a)
    }

    fn i32(&mut self) -> i32 {
        let bytes = self.read(4);
        if bytes.len() < 4 {
            return 0;
        }
        let mut a = [0u8; 4];
        a.copy_from_slice(bytes);
        i32::from_le_bytes(a)
    }

    fn string(&mut self) -> String {
        let len = self.i32();
        if len < 0 || self.failed {
            self.failed = true;
            return String::new();
        }
        let bytes = self.read(len as usize);
        if bytes.len() < len as usize {
            return String::new();
        }
        String::from_utf8_lossy(bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three key sequences an interpolated channel carries.
    type InterpolatedKeys = (
        Vec<(f32, Vector3)>,
        Vec<(f32, Vector3)>,
        Vec<(f32, Quaternion)>,
    );

    fn push_f32(bytes: &mut Vec<u8>, value: f32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_i32(bytes: &mut Vec<u8>, value: i32) {
        bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn push_string(bytes: &mut Vec<u8>, value: &str) {
        push_i32(bytes, value.len() as i32);
        bytes.extend_from_slice(value.as_bytes());
    }

    /// A rigid world transform: rotation around Y plus a translation,
    /// composed the way the exporter's matrices are.
    fn rigid(angle: f32, tx: f32, ty: f32, tz: f32) -> Matrix {
        let mut rotation = Matrix::identity();
        rotation.set_rotate_y(angle);
        rotation.m[3][0] = tx;
        rotation.m[3][1] = ty;
        rotation.m[3][2] = tz;
        rotation
    }

    #[test]
    fn animation_name_crops_path_and_extension() {
        assert_eq!(animation_name("a/b/c/hero_stand.animation"), "hero_stand");
        assert_eq!(animation_name("hero.animation"), "hero");
        assert_eq!(animation_name("hero_walk"), "hero_walk");
        // A separator at index 0 or a dot at index 0: the reference's
        // `> 0` guards leave the name whole.
        assert_eq!(animation_name("x.animation"), "x");
        assert_eq!(animation_name("a\\b\\x.animation"), "x");
    }

    #[test]
    fn the_root_channel_is_the_identity_pose() {
        // The exported root is `Scene Root`, detached from the sampler:
        // its channel is one identity key per frame — the fixture shape
        // every shipped `.animation` opens with.
        let mut root = ExportNode::new("anything");
        root.set_include(true);
        let settings = ExportSettings {
            first_frame: 0,
            last_frame: 0,
            ..ExportSettings::default()
        };
        let bytes = export(
            &mut root,
            |_identifier, _frame| Some(Matrix::identity()),
            &settings,
            None,
            CompressionInfo::default(),
            "stand",
        );

        let mut expected = Vec::new();
        push_f32(&mut expected, 0.0); // totalTime = lastFrame - firstFrame
        push_string(&mut expected, "stand");
        push_string(&mut expected, "stand");
        push_i32(&mut expected, 1); // one channel
        push_i32(&mut expected, 1); // type 1
        push_string(&mut expected, "Scene Root");
        push_i32(&mut expected, 1); // scale keys
        push_f32(&mut expected, 0.0);
        push_f32(&mut expected, 1.0);
        push_f32(&mut expected, 1.0);
        push_f32(&mut expected, 1.0);
        push_i32(&mut expected, 1); // position keys
        push_f32(&mut expected, 0.0);
        push_f32(&mut expected, 0.0);
        push_f32(&mut expected, 0.0);
        push_f32(&mut expected, 0.0);
        push_i32(&mut expected, 1); // rotation keys
        push_f32(&mut expected, 0.0);
        push_f32(&mut expected, 0.0);
        push_f32(&mut expected, 0.0);
        push_f32(&mut expected, 0.0);
        push_f32(&mut expected, 1.0);
        for _ in 0..3 {
            push_i32(&mut expected, 1); // bound table
            push_i32(&mut expected, 1);
        }
        assert_eq!(bytes, expected);
    }

    #[test]
    fn an_export_with_no_channels_writes_nothing() {
        let mut root = ExportNode::new("root");
        let settings = ExportSettings {
            export_node_animation: false,
            ..ExportSettings::default()
        };
        let bytes = export(
            &mut root,
            |_identifier, _frame| None,
            &settings,
            None,
            CompressionInfo::default(),
            "stand",
        );
        assert!(bytes.is_empty());
    }

    #[test]
    fn munge_included_marks_the_ancestors_of_every_included_node() {
        let mut root = ExportNode::new("root");
        let mut a = ExportNode::new("a");
        let mut b = ExportNode::new("b");
        let mut c = ExportNode::new("c");
        c.set_include(true);
        b.add_child(c);
        a.add_child(b);
        root.add_child(a);

        assert!(root.munge_included());
        assert!(root.find("a").unwrap().include());
        assert!(root.find("b").unwrap().include());
        assert!(root.find("c").unwrap().include());
        assert_eq!(root.n_included_nodes(), 4);
    }

    #[test]
    fn munge_included_leaves_an_excluded_subtree_alone() {
        let mut root = ExportNode::new("root");
        root.set_include(true);
        let mut helper = ExportNode::new("helper");
        helper.add_child(ExportNode::new("helper_child"));
        root.add_child(helper);

        assert!(root.munge_included());
        assert!(!root.find("helper").unwrap().include());
        assert!(!root.find("helper_child").unwrap().include());
        assert_eq!(root.n_included_nodes(), 1);
    }

    #[test]
    fn the_root_children_are_sorted_by_descending_child_count() {
        // Two included bones with different child counts: the export
        // writes the larger family first (the `numChildDescending`
        // re-order), and the equally-sized pair keeps its scene order.
        let mut root = ExportNode::new("root");
        root.set_include(true);

        let mut small = ExportNode::new("small");
        small.set_include(true);
        small.add_child({
            let mut child = ExportNode::new("small_child");
            child.set_include(true);
            child
        });

        let mut equal_a = ExportNode::new("equal_a");
        equal_a.set_include(true);
        let mut equal_b = ExportNode::new("equal_b");
        equal_b.set_include(true);

        let mut large = ExportNode::new("large");
        large.set_include(true);
        for name in ["l1", "l2"] {
            let mut child = ExportNode::new(name);
            child.set_include(true);
            large.add_child(child);
        }

        // Scene order: equal_a, equal_b, large, small.
        root.add_child(equal_a);
        root.add_child(equal_b);
        root.add_child(large);
        root.add_child(small);

        let settings = ExportSettings {
            first_frame: 0,
            last_frame: 0,
            ..ExportSettings::default()
        };
        let bytes = export(
            &mut root,
            |_identifier, _frame| Some(Matrix::identity()),
            &settings,
            None,
            CompressionInfo::default(),
            "x",
        );

        let mut r = Reader::new(&bytes);
        let _total = r.f32();
        let _name = r.string();
        let _internal = r.string();
        let count = r.i32();
        assert_eq!(count, 8); // root + 4 bones + 3 descendants
        let mut order = Vec::new();
        for _ in 0..count {
            let ty = r.i32();
            assert_eq!(ty, 1);
            order.push(r.string());
            // Skip the key sequences (4 floats per scale/position key,
            // 5 per rotation key) and the three bound tables.
            skip_vec3_seq(&mut r);
            skip_vec3_seq(&mut r);
            skip_quat_seq(&mut r);
            skip_i32_seq(&mut r);
            skip_i32_seq(&mut r);
            skip_i32_seq(&mut r);
        }
        // large (2 children) first, then the 1-child family, then the
        // two 0-child nodes in scene order (stable sort); each family's
        // channels follow the preorder walk.
        assert_eq!(
            order,
            vec![
                "Scene Root",
                "large",
                "l1",
                "l2",
                "small",
                "small_child",
                "equal_a",
                "equal_b"
            ]
        );
    }

    fn skip_vec3_seq(r: &mut Reader) {
        let count = r.i32();
        for _ in 0..count {
            for _ in 0..4 {
                r.f32();
            }
        }
    }

    fn skip_quat_seq(r: &mut Reader) {
        let count = r.i32();
        for _ in 0..count {
            for _ in 0..5 {
                r.f32();
            }
        }
    }

    fn skip_i32_seq(r: &mut Reader) {
        let count = r.i32();
        for _ in 0..count {
            r.i32();
        }
    }

    /// The row-vector chain: a child's world is its local relative
    /// transform followed by the parent's world.
    fn chain(rel: &Matrix, parent_world: &Matrix) -> Matrix {
        let mut out = Matrix::zero();
        out.multiply(rel, parent_world);
        out
    }

    /// A translation-only local transform.
    fn translate(x: f32, y: f32, z: f32) -> Matrix {
        let mut m = Matrix::identity();
        m.m[3][0] = x;
        m.m[3][1] = y;
        m.m[3][2] = z;
        m
    }

    /// The expected rotation of a Y-rotation after the exporter's axis
    /// permutation, written from the spec (`utility.cpp:42-53`: columns
    /// one and two trade, then basis rows one and two trade) rather than
    /// the implementation.
    fn rearranged_y_rotation(angle: f32) -> Matrix {
        let mut ry = Matrix::identity();
        ry.set_rotate_y(angle);
        let mut m = Matrix::identity();
        for r in 0..3 {
            m.m[r][0] = ry.m[r][0];
            m.m[r][1] = ry.m[r][2];
            m.m[r][2] = ry.m[r][1];
        }
        for c in 0..3 {
            let row2 = m.m[2][c];
            m.m[2][c] = m.m[1][c];
            m.m[1][c] = row2;
        }
        m
    }

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "actual {actual}, expected {expected}"
        );
    }

    fn assert_vec3_close(actual: &Vector3, expected: &Vector3) {
        assert_close(actual.x, expected.x);
        assert_close(actual.y, expected.y);
        assert_close(actual.z, expected.z);
    }

    fn assert_quat_rebuilds_rotation(q: &Quaternion, expected: &Matrix) {
        let mut rebuilt = Matrix::identity();
        rebuilt.set_rotate_quat(q);
        for (row, expected_row) in rebuilt.m.iter().zip(expected.m.iter()).take(3) {
            for (cell, expected_cell) in row.iter().zip(expected_row).take(3) {
                assert_close(*cell, *expected_cell);
            }
        }
    }

    #[test]
    fn the_written_header_matches_the_shipped_fixture() {
        // Anchored against the files the Max exporter really shipped
        // (`crab_walkforward.animation`): `totalTime`, the animation
        // name twice, the channel count, then the root's plain type-1
        // head. A round trip through the engine's reader could agree
        // with the writer while both disagreed with the fixtures, so
        // the file is what pins the layout.
        let shipped = include_bytes!("../../client/res/crab_walkforward.animation");
        let mut header = Writer::default();
        header.f32(25.0);
        header.string("crab_walkforward");
        header.string("crab_walkforward");
        header.i32(45);
        header.i32(1);
        header.string("Scene Root");
        assert_eq!(&shipped[..header.bytes.len()], &header.bytes[..]);
    }

    /// The acceptance: 骨骼与时间关键帧一致 — the exported file's skeleton
    /// (one channel per included node, preorder, the excluded subtree
    /// absent) and its time keyframes (per-frame times from zero, the
    /// decomposed scale/translation/rotation) read back through the
    /// engine's `.animation` reader.
    #[test]
    fn the_export_reads_back_with_matching_skeleton_and_time_keyframes() {
        // root → Bip01 → { Spine, CamHelper → CamHelper2 }; CamHelper's
        // subtree is excluded and writes no channel.
        let mut root = ExportNode::new("the_root");
        root.set_include(true);

        let mut bip01 = ExportNode::new("Bip01");
        bip01.set_include(true);

        let mut spine = ExportNode::new("Bip01 Spine");
        spine.set_include(true);

        let mut cam_helper = ExportNode::new("CamHelper");
        cam_helper.add_child(ExportNode::new("CamHelper2"));
        bip01.add_child(spine);
        bip01.add_child(cam_helper);
        root.add_child(bip01);

        let settings = ExportSettings {
            first_frame: 2,
            last_frame: 5,
            allow_scale: false,
            export_node_animation: true,
            export_cue_track: false,
            use_legacy_orientation: false,
            unit_scale: 2.0,
        };

        let angle = |frame: i32| 0.1 * frame as f32;
        let bip01_world = |frame: i32| rigid(angle(frame), 0.0, 0.25 * frame as f32, 0.0);
        let spine_world = |frame: i32| chain(&translate(0.0, 1.0, 0.0), &bip01_world(frame));
        let sample = |identifier: &str, frame: i32| -> Option<Matrix> {
            match identifier {
                "Bip01" => Some(bip01_world(frame)),
                "Bip01 Spine" => Some(spine_world(frame)),
                _ => None,
            }
        };

        let bytes = export(
            &mut root,
            sample,
            &settings,
            None,
            CompressionInfo::default(),
            "stand",
        );
        let animation = client::skeleton::Animation::from_bytes(&bytes)
            .expect("the exported bytes load as an animation");

        // The header: the total time is lastFrame - firstFrame, the name
        // twice, and the channels are exactly the included nodes.
        assert_close(animation.total_time(), 3.0);
        assert_eq!(animation.identifier(), "stand");
        assert_eq!(animation.internal_identifier(), "stand");
        assert_eq!(animation.channel_count(), 3);
        let identifiers: Vec<&str> = animation
            .channels()
            .iter()
            .map(|c| c.identifier())
            .collect();
        assert_eq!(identifiers, vec!["Scene Root", "Bip01", "Bip01 Spine"]);

        let rotation_of = |identifier: &str| -> &client::skeleton::Channel {
            animation
                .channels()
                .iter()
                .find(|c| c.identifier() == identifier)
                .unwrap()
        };

        // The root: the detached identity pose, one key per frame.
        let root_channel = rotation_of("Scene Root");
        let client::skeleton::Channel::Interpolated {
            scale_keys,
            position_keys,
            rotation_keys,
            ..
        } = root_channel
        else {
            panic!("the root channel is an interpolated channel");
        };
        assert_eq!(scale_keys.len(), 4);
        assert_eq!(position_keys.len(), 4);
        assert_eq!(rotation_keys.len(), 4);
        for (index, (time, scale)) in scale_keys.iter().enumerate() {
            assert_close(*time, index as f32);
            assert_vec3_close(scale, &Vector3::new(1.0, 1.0, 1.0));
        }
        for (index, (time, position)) in position_keys.iter().enumerate() {
            assert_close(*time, index as f32);
            assert_vec3_close(position, &Vector3::new(0.0, 0.0, 0.0));
        }
        for (index, (time, rotation)) in rotation_keys.iter().enumerate() {
            assert_close(*time, index as f32);
            assert_close(rotation.x, 0.0);
            assert_close(rotation.y, 0.0);
            assert_close(rotation.z, 0.0);
            assert_close(rotation.w, 1.0);
        }

        // Bip01: its own world against the root's identity — the
        // translation scales by the unit scale, the rotation is the
        // rearranged Y rotation.
        let bip01_channel = rotation_of("Bip01");
        let client::skeleton::Channel::Interpolated {
            scale_keys,
            position_keys,
            rotation_keys,
            ..
        } = bip01_channel
        else {
            panic!("Bip01 is an interpolated channel");
        };
        for (index, (time, scale)) in scale_keys.iter().enumerate() {
            assert_close(*time, index as f32);
            assert_vec3_close(scale, &Vector3::new(1.0, 1.0, 1.0));
        }
        for (index, (time, position)) in position_keys.iter().enumerate() {
            let frame = settings.first_frame + index as i32;
            assert_close(*time, index as f32);
            // The unit scale doubles the translation row.
            assert_vec3_close(position, &Vector3::new(0.0, 0.5 * frame as f32, 0.0));
        }
        for (index, (time, rotation)) in rotation_keys.iter().enumerate() {
            let frame = settings.first_frame + index as i32;
            assert_close(*time, index as f32);
            assert_quat_rebuilds_rotation(rotation, &rearranged_y_rotation(angle(frame)));
        }

        // Spine: its local transform against Bip01's world — the keys are
        // the constant (0, 1, 0) translation (scaled) and identity
        // rotation, even though its own world moves.
        let spine_channel = rotation_of("Bip01 Spine");
        let client::skeleton::Channel::Interpolated {
            scale_keys,
            position_keys,
            rotation_keys,
            ..
        } = spine_channel
        else {
            panic!("Spine is an interpolated channel");
        };
        assert_eq!(scale_keys.len(), 4);
        assert_eq!(position_keys.len(), 4);
        assert_eq!(rotation_keys.len(), 4);
        for (time, scale) in scale_keys.iter() {
            assert_vec3_close(scale, &Vector3::new(1.0, 1.0, 1.0));
            let _ = time;
        }
        for (time, position) in position_keys.iter() {
            assert_vec3_close(position, &Vector3::new(0.0, 2.0, 0.0));
            let _ = time;
        }
        for (time, rotation) in rotation_keys.iter() {
            assert_close(rotation.x, 0.0);
            assert_close(rotation.y, 0.0);
            assert_close(rotation.z, 0.0);
            assert_close(rotation.w, 1.0);
            let _ = time;
        }
    }

    #[test]
    fn a_hierarchy_only_node_carries_the_identity_pose() {
        // `getRelativeTransform` (`mfxnode.cpp:105-120`): a node without
        // a Max node returns its stored transform — the constructor's
        // identity, and `setTransform` has no call site — whatever its
        // parent's world. A sampled node below it still resolves against
        // that parent's world: `getTransform`'s parent arm multiplies
        // the identity in, so the chain answers the nearest sampled
        // ancestor's world.
        let mut root = ExportNode::new("root");
        root.set_include(true);
        let mut bip01 = ExportNode::new("Bip01");
        bip01.set_include(true);
        let mut phantom = ExportNode::new("Phantom");
        phantom.set_include(true);
        let mut ghost = ExportNode::new("Ghost");
        ghost.set_include(true);
        phantom.add_child(ghost);
        bip01.add_child(phantom);
        root.add_child(bip01);

        let settings = ExportSettings {
            first_frame: 0,
            last_frame: 2,
            ..ExportSettings::default()
        };
        let bip01_world = |frame: i32| rigid(0.0, 0.0, 0.5 * frame as f32, 0.0);
        let ghost_world = |frame: i32| rigid(0.0, 0.0, frame as f32, 0.0);
        let bytes = export(
            &mut root,
            |identifier, frame| match identifier {
                "Bip01" => Some(bip01_world(frame)),
                "Ghost" => Some(ghost_world(frame)),
                _ => None,
            },
            &settings,
            None,
            CompressionInfo::default(),
            "x",
        );
        let animation = client::skeleton::Animation::from_bytes(&bytes).unwrap();
        assert_eq!(animation.channel_count(), 4);

        let keys_of = |identifier: &str| -> InterpolatedKeys {
            match animation
                .channels()
                .iter()
                .find(|c| c.identifier() == identifier)
                .unwrap()
            {
                client::skeleton::Channel::Interpolated {
                    scale_keys,
                    position_keys,
                    rotation_keys,
                    ..
                } => (
                    scale_keys.clone(),
                    position_keys.clone(),
                    rotation_keys.clone(),
                ),
                _ => panic!("{identifier} is an interpolated channel"),
            }
        };

        // Phantom never samples: the identity pose across the range.
        let (scale_keys, position_keys, rotation_keys) = keys_of("Phantom");
        assert_eq!(scale_keys.len(), 3);
        assert_eq!(position_keys.len(), 3);
        assert_eq!(rotation_keys.len(), 3);
        for (index, (time, scale)) in scale_keys.iter().enumerate() {
            assert_close(*time, index as f32);
            assert_vec3_close(scale, &Vector3::new(1.0, 1.0, 1.0));
        }
        for (index, (time, position)) in position_keys.iter().enumerate() {
            assert_close(*time, index as f32);
            assert_vec3_close(position, &Vector3::new(0.0, 0.0, 0.0));
        }
        for (index, (time, rotation)) in rotation_keys.iter().enumerate() {
            assert_close(*time, index as f32);
            assert_close(rotation.w, 1.0);
        }

        // Ghost samples: its world against the hierarchy-only parent's —
        // the parent's world is Bip01's — so the keys are the half-step
        // offsets with the identity rotation.
        let (scale_keys, position_keys, rotation_keys) = keys_of("Ghost");
        for (index, (time, scale)) in scale_keys.iter().enumerate() {
            assert_close(*time, index as f32);
            assert_vec3_close(scale, &Vector3::new(1.0, 1.0, 1.0));
        }
        for (index, (time, position)) in position_keys.iter().enumerate() {
            assert_close(*time, index as f32);
            assert_vec3_close(position, &Vector3::new(0.0, 0.5 * index as f32, 0.0));
        }
        for (index, (time, rotation)) in rotation_keys.iter().enumerate() {
            assert_close(*time, index as f32);
            assert_close(rotation.w, 1.0);
        }
    }

    #[test]
    fn scale_keys_carry_the_row_lengths_and_the_z_flip() {
        // A mirrored basis: the z row points the wrong way, so the
        // decomposition flips it — the scale's z goes negative and the
        // rotation absorbs the mirror as a half turn.
        let mut root = ExportNode::new("root");
        root.set_include(true);
        let mut bone = ExportNode::new("Bone");
        bone.set_include(true);
        root.add_child(bone);

        let settings = ExportSettings {
            first_frame: 0,
            last_frame: 0,
            allow_scale: true,
            ..ExportSettings::default()
        };
        let mut world = Matrix::identity();
        world.m[0][0] = 2.0;
        world.m[1][1] = 3.0;
        world.m[2][2] = -4.0;
        world.m[3][0] = 1.0;

        let bytes = export(
            &mut root,
            |identifier, _frame| (identifier == "Bone").then_some(world),
            &settings,
            None,
            CompressionInfo::default(),
            "x",
        );
        let animation = client::skeleton::Animation::from_bytes(&bytes).unwrap();
        match animation
            .channels()
            .iter()
            .find(|c| c.identifier() == "Bone")
            .unwrap()
        {
            client::skeleton::Channel::Interpolated {
                scale_keys,
                position_keys,
                rotation_keys,
                ..
            } => {
                // The rearrange trades the basis columns before the
                // decomposition reads the row lengths, so diag(2, 3, -4)
                // arrives as diag(2, -4, 3) — the flip lands on z.
                assert_vec3_close(&scale_keys[0].1, &Vector3::new(2.0, 4.0, -3.0));
                assert_vec3_close(&position_keys[0].1, &Vector3::new(1.0, 0.0, 0.0));
                // The flip turns the mirrored basis into diag(1, -1, -1):
                // the half-turn about X, not the identity.
                let half_turn_x = {
                    let mut m = Matrix::identity();
                    m.m[1][1] = -1.0;
                    m.m[2][2] = -1.0;
                    m
                };
                assert_quat_rebuilds_rotation(&rotation_keys[0].1, &half_turn_x);
            }
            _ => panic!("Bone is an interpolated channel"),
        }
    }

    #[test]
    fn normalising_strips_the_scale_from_the_worlds() {
        let mut root = ExportNode::new("root");
        root.set_include(true);
        let mut bone = ExportNode::new("Bone");
        bone.set_include(true);
        root.add_child(bone);

        // allowScale false: the world is normalised before the relative
        // step, so the scale keys are the unit scale.
        let settings = ExportSettings {
            first_frame: 0,
            last_frame: 0,
            allow_scale: false,
            ..ExportSettings::default()
        };
        let mut world = Matrix::identity();
        world.m[0][0] = 2.0;
        world.m[1][1] = 3.0;
        world.m[2][2] = 4.0;

        let bytes = export(
            &mut root,
            |identifier, _frame| (identifier == "Bone").then_some(world),
            &settings,
            None,
            CompressionInfo::default(),
            "x",
        );
        let animation = client::skeleton::Animation::from_bytes(&bytes).unwrap();
        match animation
            .channels()
            .iter()
            .find(|c| c.identifier() == "Bone")
            .unwrap()
        {
            client::skeleton::Channel::Interpolated { scale_keys, .. } => {
                assert_vec3_close(&scale_keys[0].1, &Vector3::new(1.0, 1.0, 1.0));
            }
            _ => panic!("Bone is an interpolated channel"),
        }
    }

    #[test]
    fn the_legacy_orientation_negates_the_permuted_basis() {
        let mut root = ExportNode::new("root");
        root.set_include(true);
        let mut bone = ExportNode::new("Bone");
        bone.set_include(true);
        root.add_child(bone);

        let settings = ExportSettings {
            first_frame: 0,
            last_frame: 0,
            allow_scale: true,
            use_legacy_orientation: true,
            ..ExportSettings::default()
        };
        let world = rigid(0.3, 1.0, 2.0, 3.0);
        let bytes = export(
            &mut root,
            |identifier, _frame| (identifier == "Bone").then_some(world),
            &settings,
            None,
            CompressionInfo::default(),
            "x",
        );
        let animation = client::skeleton::Animation::from_bytes(&bytes).unwrap();
        match animation
            .channels()
            .iter()
            .find(|c| c.identifier() == "Bone")
            .unwrap()
        {
            client::skeleton::Channel::Interpolated {
                position_keys,
                rotation_keys,
                ..
            } => {
                // The translation is carried either way.
                assert_vec3_close(&position_keys[0].1, &Vector3::new(1.0, 2.0, 3.0));
                // The legacy basis negations, hand-built from the spec
                // (rows 2 and 0 negated, then columns 2 and 0).
                let expected = {
                    let mut m = rearranged_y_rotation(0.3);
                    for c in 0..3 {
                        m.m[2][c] = -m.m[2][c];
                        m.m[0][c] = -m.m[0][c];
                    }
                    for row in m.m.iter_mut().take(3) {
                        row[2] = -row[2];
                        row[0] = -row[0];
                    }
                    m
                };
                assert_quat_rebuilds_rotation(&rotation_keys[0].1, &expected);
            }
            _ => panic!("Bone is an interpolated channel"),
        }
    }

    #[test]
    fn compression_amounts_round_trip_from_the_written_file() {
        let mut root = ExportNode::new("root");
        root.set_include(true);
        let compression = CompressionInfo {
            specify_amounts: true,
            scale_compression_error: 0.01,
            position_compression_error: 0.02,
            rotation_compression_error: 0.005,
        };
        let settings = ExportSettings {
            first_frame: 0,
            last_frame: 0,
            ..ExportSettings::default()
        };
        let bytes = export(
            &mut root,
            |_identifier, _frame| Some(Matrix::identity()),
            &settings,
            None,
            compression,
            "x",
        );

        // The written channel is a type 4 and carries the amounts.
        let read = read_compression_info(&bytes);
        assert_eq!(read, compression);

        // And the reader consumes the type-4 head.
        let animation = client::skeleton::Animation::from_bytes(&bytes).unwrap();
        assert_eq!(animation.channel_count(), 1);
    }

    #[test]
    fn read_compression_info_defaults_on_foreign_input() {
        // Truncated, empty and type-1-channel files all leave the
        // amounts unspecified.
        assert_eq!(read_compression_info(&[]), CompressionInfo::default());
        let mut bytes = Vec::new();
        push_f32(&mut bytes, 1.0);
        assert_eq!(read_compression_info(&bytes), CompressionInfo::default());

        let mut plain = Vec::new();
        push_f32(&mut plain, 0.0);
        push_string(&mut plain, "x");
        push_string(&mut plain, "x");
        push_i32(&mut plain, 1);
        push_i32(&mut plain, 1); // type 1
        push_string(&mut plain, "Scene Root");
        assert_eq!(read_compression_info(&plain), CompressionInfo::default());

        // A type-4 head truncated mid-way through the amounts.
        let mut cut = Vec::new();
        push_f32(&mut cut, 0.0);
        push_string(&mut cut, "x");
        push_string(&mut cut, "x");
        push_i32(&mut cut, 1);
        push_i32(&mut cut, 4);
        push_string(&mut cut, "Scene Root");
        push_f32(&mut cut, 0.01);
        assert_eq!(read_compression_info(&cut), CompressionInfo::default());
    }

    #[test]
    fn cues_filter_frames_prefixes_and_duplicates() {
        let mut track = CueTrack::default();
        track.add_cue(1, "cue:early", 2, 5); // before the range
        track.add_cue(6, "cue:late", 2, 5); // after the range
        track.add_cue(2, "cue:hit", 2, 5);
        track.add_cue(2, "cue:hit", 2, 5); // duplicate
        track.add_cue(2, "sound:step", 2, 5);
        track.add_cue(3, "zap:unknown", 2, 5); // unknown prefix
        track.add_cue(3, "nocolon", 2, 5); // no separator
        track.add_cue(4, "cue:again", 2, 5);

        let cues: Vec<(i32, Vec<String>)> = track
            .cues()
            .map(|(time, entry)| (time, entry.to_vec()))
            .collect();
        assert_eq!(
            cues,
            vec![
                (0, vec!["chit".to_string(), "sstep".to_string()]),
                (2, vec!["cagain".to_string()]),
            ]
        );
        assert!(track.has_cues());
        track.clear();
        assert!(!track.has_cues());
    }

    #[test]
    fn the_cue_track_reads_back_as_the_last_channel() {
        let mut root = ExportNode::new("root");
        root.set_include(true);

        let mut track = CueTrack::default();
        track.add_cue(3, "cue:hit", 2, 4);
        track.add_cue(4, "sound:step", 2, 4);

        let settings = ExportSettings {
            first_frame: 2,
            last_frame: 4,
            export_cue_track: true,
            ..ExportSettings::default()
        };
        let bytes = export(
            &mut root,
            |_identifier, _frame| Some(Matrix::identity()),
            &settings,
            Some(&track),
            CompressionInfo::default(),
            "x",
        );
        let animation = client::skeleton::Animation::from_bytes(&bytes).unwrap();

        // The root channel plus the cue channel, in that order.
        assert_eq!(animation.channel_count(), 2);
        match &animation.channels()[1] {
            client::skeleton::Channel::Cue { cues } => {
                assert_eq!(cues.len(), 2);
                // The cue at frame 3 fires one frame into the animation.
                assert_close(cues[0].0, 1.0);
                assert_eq!(cues[0].1, "chit");
                assert!(cues[0].2.is_empty());
                assert_close(cues[1].0, 2.0);
                assert_eq!(cues[1].1, "sstep");
                assert!(cues[1].2.is_empty());
            }
            other => panic!("the second channel is the cue channel, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_cue_track_writes_no_channel() {
        let mut root = ExportNode::new("root");
        root.set_include(true);
        let track = CueTrack::default();

        let settings = ExportSettings {
            first_frame: 0,
            last_frame: 0,
            export_cue_track: true,
            ..ExportSettings::default()
        };
        let bytes = export(
            &mut root,
            |_identifier, _frame| Some(Matrix::identity()),
            &settings,
            Some(&track),
            CompressionInfo::default(),
            "x",
        );
        let animation = client::skeleton::Animation::from_bytes(&bytes).unwrap();
        assert_eq!(animation.channel_count(), 1);
    }

    #[test]
    fn whitespace_names_are_flagged() {
        assert!(has_trailing_leading_whitespace(" Bip01"));
        assert!(has_trailing_leading_whitespace("Bip01 "));
        assert!(has_trailing_leading_whitespace(" "));
        assert!(!has_trailing_leading_whitespace("Bip01"));
        assert!(!has_trailing_leading_whitespace(""));
        assert!(!has_trailing_leading_whitespace("Bip 01"));
    }

    #[test]
    fn tree_census_faces_walk_the_owning_tree() {
        let mut root = ExportNode::new("root");
        let mut a = ExportNode::new("a");
        a.add_child(ExportNode::new("a1"));
        root.add_child(a);
        root.add_child(ExportNode::new("b"));

        assert_eq!(root.tree_size(), 4);
        assert_eq!(root.n_children(), 2);
        assert_eq!(root.find("a1").unwrap().identifier(), "a1");
        assert!(root.find("missing").is_none());

        let child = &mut root.children_mut()[0].children_mut()[0];
        child.set_identifier("renamed");
        assert_eq!(root.find("renamed").unwrap().identifier(), "renamed");
    }
}
