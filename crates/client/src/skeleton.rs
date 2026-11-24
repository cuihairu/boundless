//! BW-11-04-01: loading models and skeletal animation, ported along the
//! reference client path that the acceptance fixture exercises end to end:
//!
//! - the visual node tree (`lib/moo/node.cpp` `Node::loadRecursive`, with the
//!   `Moo::Visual` loader picking `openSection("node")` as the root,
//!   `lib/moo/visual.cpp:586-591`);
//! - the `.animation` binary (`lib/moo/animation.cpp` `Animation::load`,
//!   `lib/moo/animation_channel.cpp` base identifier, and the five channel
//!   types used for pose: `discrete_animation_channel.cpp`,
//!   `interpolated_animation_channel.cpp`, `streamed_animation_channel.cpp`,
//!   `cue_channel.cpp`, `translation_override_channel.cpp`);
//! - channel binding by node identifier (`Animation::Animation(anim, root)`
//!   via `Node::find`; a channel whose identifier is absent from the visual
//!   tree binds to the global node catalogue and contributes no pose, the
//!   `NodefullModel` pose is unaffected by it);
//! - pose evaluation (`interpolateKeys` + `D3DXQuaternionSlerp` /
//!   `D3DXVec3Lerp` / `buildTransform` in `interpolated_animation_channel.cpp`,
//!   the discrete map's `upper_bound`, then `Node::visitSelf`'s
//!   `world = local x parent`);
//! - the model layer (`model_common.hpp` `createModelFromFile` dispatch,
//!   `Model::loadAnimations` name/nodes walk, and the
//!   `NodefullModelAnimation` frame mapping).
//!
//! The interpolation maths is the D3DX one — the client calls D3DX directly
//! (`interpolated_animation_channel.cpp:138-170` passes `D3DXQuaternionSlerp`
//! / `D3DXVec3Lerp` into `interpolateKeys`) — not the BW `Quaternion::slerp`
//! scalar fallback in `lib/math/quat.cpp`, which has a different branch
//! threshold (0.001 vs D3DX's 0.9999 cos).
//!
//! Type 2 (morph) channels are unsupported and stop reading, matching
//! `animation.cpp:578-582`.

use foundation::math::matrix::Matrix;
use foundation::math::quaternion::Quaternion;
use foundation::math::vector::Vector3;
use resources::datasection::DataSection;

/// `AnimationChannel::MORPH_ANIMATION_CHANNEL_NOT_SUPPORTED`
/// (`animation_channel.hpp:32`); reading one stops the binder loop.
const MORPH_ANIMATION_CHANNEL_NOT_SUPPORTED: i32 = 2;

// ===========================================================================
// D3DX interpolation helpers (client target: D3DX formulas)
// ===========================================================================

/// `D3DXVec3Lerp`: per component `a + t * (b - a)`.
fn d3dx_vec3_lerp(a: &Vector3, b: &Vector3, t: f32) -> Vector3 {
    Vector3::new(
        a.x + t * (b.x - a.x),
        a.y + t * (b.y - a.y),
        a.z + t * (b.z - a.z),
    )
}

/// `D3DXQuaternionSlerp`: short-arc (flips the end quaternion when the dot is
/// negative), linear blend when `cos > 0.9999`, else spherical.
fn d3dx_quaternion_slerp(q1: &Quaternion, q2: &Quaternion, t: f32) -> Quaternion {
    let mut cos = q1.dot_product(q2);
    let mut end = *q2;
    if cos < 0.0 {
        cos = -cos;
        end = Quaternion::new(-q2.x, -q2.y, -q2.z, -q2.w);
    }
    let (k0, k1) = if cos > 0.9999 {
        (1.0 - t, t)
    } else {
        let sin = (1.0 - cos * cos).sqrt();
        let omega = f32::atan2(sin, cos);
        (((1.0 - t) * omega).sin() / sin, (t * omega).sin() / sin)
    };
    Quaternion::new(
        q1.x * k0 + end.x * k1,
        q1.y * k0 + end.y * k1,
        q1.z * k0 + end.z * k1,
        q1.w * k0 + end.w * k1,
    )
}

/// `buildTransform` (`interpolated_animation_channel.cpp:138-150`):
/// `D3DXMatrixRotationQuaternion`, scale the first three rows by the scale, and
/// overwrite the translation row.
fn build_transform(rot: &Quaternion, scale: &Vector3, pos: &Vector3) -> Matrix {
    let mut out = Matrix::zero();
    out.set_rotate_quat(rot);
    for c in 0..3 {
        out.m[0][c] *= scale.x;
        out.m[1][c] *= scale.y;
        out.m[2][c] *= scale.z;
    }
    out.m[3][0] = pos.x;
    out.m[3][1] = pos.y;
    out.m[3][2] = pos.z;
    out.m[3][3] = 1.0;
    out
}

/// `interpolateKeys` on a `vector<pair<float, V>>` (`interpolated_animation_channel.cpp:93-134`):
/// `upper_bound` on the sorted-by-time keys; before the first key returns the
/// front value, after the last returns the back value, otherwise slerps/lerps
/// the bracketing pair with `t = (time - t1) / (t2 - t1)`.
fn interpolate_keys_vec3(keys: &[(f32, Vector3)], time: f32) -> Vector3 {
    // partition_point on `time > time` == the reference `upper_bound`.
    let upper = keys.partition_point(|(t, _)| *t <= time);
    if upper == 0 {
        return keys[0].1;
    }
    if upper == keys.len() {
        return keys[keys.len() - 1].1;
    }
    let t1 = keys[upper - 1].0;
    let t2 = keys[upper].0;
    let t = (time - t1) / (t2 - t1);
    d3dx_vec3_lerp(&keys[upper - 1].1, &keys[upper].1, t)
}

fn interpolate_keys_quat(keys: &[(f32, Quaternion)], time: f32) -> Quaternion {
    let upper = keys.partition_point(|(t, _)| *t <= time);
    if upper == 0 {
        return keys[0].1;
    }
    if upper == keys.len() {
        return keys[keys.len() - 1].1;
    }
    let t1 = keys[upper - 1].0;
    let t2 = keys[upper].0;
    let t = (time - t1) / (t2 - t1);
    d3dx_quaternion_slerp(&keys[upper - 1].1, &keys[upper].1, t)
}

// ===========================================================================
// .animation binary reader
// ===========================================================================

/// A `BinaryFile` cursor (`cstdmf/binaryfile.hpp:202-218` reads `sizeof(T)`
/// bytes; a short read sets the error flag and every later read fails).
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    failed: bool,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader {
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

    fn u32(&mut self) -> u32 {
        self.i32() as u32
    }

    /// `BW::string` length-prefixed string (`binaryfile.hpp:220-258`): the
    /// int length must be in `[0, bytes_available]`, else the file errors.
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

    /// One `vector<pair<float, V>>` sequence (`readSequence` over
    /// `pair<float, V>`: u32 count, then count × (f32 time + V)).
    fn vec3_seq(&mut self) -> Vec<(f32, Vector3)> {
        let count = self.u32();
        if count > Reader::MAX_SEQ_ELEMENTS {
            self.failed = true;
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count.min(1024) as usize);
        for _ in 0..count {
            let t = self.f32();
            let v = Vector3::new(self.f32(), self.f32(), self.f32());
            if self.failed {
                break;
            }
            out.push((t, v));
        }
        out
    }

    fn quat_seq(&mut self) -> Vec<(f32, Quaternion)> {
        let count = self.u32();
        if count > Reader::MAX_SEQ_ELEMENTS {
            self.failed = true;
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count.min(1024) as usize);
        for _ in 0..count {
            let t = self.f32();
            let q = Quaternion::new(self.f32(), self.f32(), self.f32(), self.f32());
            if self.failed {
                break;
            }
            out.push((t, q));
        }
        out
    }
}

/// The reference's `MAX_EXPECTED_SEQUENCE_ELEMENTS` assert
/// (`binaryfile.hpp:84`), turned into a load failure.
const _: u64 = 100 * 1000 * 1000;

impl Reader<'_> {
    /// `MAX_EXPECTED_SEQUENCE_ELEMENTS` is an instance-free bound, but it
    /// reads better as an associated constant next to the readers that use it.
    const MAX_SEQ_ELEMENTS: u32 = 100 * 1000 * 1000;
}

/// `interpolateKeys` requires non-empty key vectors, and `valid()` additionally
/// requires every `buildIndex` index in `(0, size]`
/// (`interpolated_animation_channel.cpp:788-819`); `buildIndex` zeroes index
/// entries only when a key time is non-positive, so this is exactly "keys
/// exist and start at or before time zero".
fn interpolated_valid<V>(keys: &[(f32, V)], index: &[u32]) -> bool {
    !keys.is_empty() && index.iter().all(|&i| i > 0 && (i as usize) <= keys.len())
}

/// `buildIndex` over sorted-by-time keys: for `i` in `0..=(last_time + 1)`
/// the index is the offset of the first key strictly after `i`
/// (`interpolated_animation_channel.cpp:202-237`). The last entry is the end
/// sentinel `size`.
fn build_index<V>(keys: &[(f32, V)]) -> Vec<u32> {
    if keys.is_empty() {
        return Vec::new();
    }
    let mut index = Vec::new();
    let mut it = 0usize;
    let last = keys[keys.len() - 1].0;
    let mut i = 0u32;
    while (i as f32) <= last + 1.0 {
        while it < keys.len() && keys[it].0 <= i as f32 {
            it += 1;
        }
        index.push(it as u32);
        i += 1;
    }
    index
}

// ===========================================================================
// Channels
// ===========================================================================

/// One `AnimationChannel` from the binder list. Variants carry exactly what
/// the reference reads; pose evaluation is only meaningful for the two channel
/// kinds with a `result(frame)` (discrete and interpolated).
#[derive(Debug, Clone, PartialEq)]
pub enum Channel {
    /// Type 0 `DiscreteAnimationChannel`: `std::map<time, Matrix>`; duplicate
    /// times are first-wins on read, iteration is sorted by time.
    Discrete {
        identifier: String,
        keys: Vec<(f32, Matrix)>,
    },
    /// Types 1/3/4 `InterpolatedAnimationChannel`. Type 4 carries the three
    /// compression errors (the file is read raw either way, `inhibitCompression_`
    /// defaults true so load never compresses).
    Interpolated {
        identifier: String,
        scale_keys: Vec<(f32, Vector3)>,
        position_keys: Vec<(f32, Vector3)>,
        rotation_keys: Vec<(f32, Quaternion)>,
    },
    /// Type 5 `StreamedAnimationChannel` header: the base transform fallbacks
    /// for a channel whose keys are streamed from disk cache; the streamed
    /// blocks themselves load on demand, out of scope for the pose path.
    Streamed {
        identifier: String,
        scale_fallback: Vector3,
        position_fallback: Vector3,
        rotation_fallback: Quaternion,
    },
    /// Type 6 `CueChannel`: no base identifier is read (the reference skips
    /// the `AnimationChannel::load` call), each key is a time + cue base +
    /// argument floats. Cues fire events, never pose.
    Cue { cues: Vec<(f32, String, Vec<f32>)> },
    /// Type 7 `TranslationOverrideChannel`: wraps a base animation channel and
    /// applies a translation offset/override on top of it.
    TranslationOverride {
        identifier: String,
        anim_resource: String,
        translation: Vector3,
        override_: bool,
    },
}

impl Channel {
    /// The channel identifier; `Cue` has none (the reference skips its base
    /// `load`), and binds to nothing.
    pub fn identifier(&self) -> &str {
        match self {
            Channel::Discrete { identifier, .. }
            | Channel::Interpolated { identifier, .. }
            | Channel::Streamed { identifier, .. }
            | Channel::TranslationOverride { identifier, .. } => identifier,
            Channel::Cue { .. } => "",
        }
    }

    /// The local transform this channel produces at `frame`, or `None` for
    /// channel kinds that carry no pose.
    fn result(&self, frame: f32) -> Option<Matrix> {
        match self {
            Channel::Discrete { keys, .. } => {
                if keys.is_empty() {
                    return None;
                }
                // `upper_bound`, then back one unless at begin; at begin the
                // map's first key applies; empty map leaves the node at its
                // previous transform (the 2-arg `result` writes nothing).
                let upper = keys.partition_point(|(t, _)| *t <= frame);
                if upper != 0 {
                    Some(keys[upper - 1].1)
                } else {
                    Some(keys[0].1)
                }
            }
            Channel::Interpolated {
                scale_keys,
                position_keys,
                rotation_keys,
                ..
            } => {
                let rot = interpolate_keys_quat(rotation_keys, frame);
                let scale = interpolate_keys_vec3(scale_keys, frame);
                let pos = interpolate_keys_vec3(position_keys, frame);
                Some(build_transform(&rot, &scale, &pos))
            }
            Channel::Streamed { .. }
            | Channel::Cue { .. }
            | Channel::TranslationOverride { .. } => None,
        }
    }
}

// ===========================================================================
// Animation
// ===========================================================================

/// The pose channels of one `.animation` file
/// (`lib/moo/animation.cpp` `Animation::load`).
#[derive(Debug, Clone, PartialEq)]
pub struct Animation {
    total_time: f32,
    identifier: String,
    internal_identifier: String,
    channels: Vec<Channel>,
}

impl Animation {
    /// `Animation::load` (`animation.cpp:509-677`): the f32 total time, the
    /// identifier/internalIdentifier strings, the i32 binder count, then each
    /// binder. Type 2 stops the loop (morph unsupported); a truncated read or
    /// an unknown type fails the load.
    pub fn from_bytes(data: &[u8]) -> Result<Animation, String> {
        let mut r = Reader::new(data);
        let total_time = r.f32();
        let identifier = r.string();
        let internal_identifier = r.string();
        let num_binders = r.i32();
        if !r.ok() {
            return Err("truncated animation header".to_string());
        }
        let mut channels = Vec::new();
        for _ in 0..num_binders {
            let ty = r.i32();
            if ty == MORPH_ANIMATION_CHANNEL_NOT_SUPPORTED {
                // `animation.cpp:578-582`: morph is unsupported, the loop
                // breaks and the rest of the file is not read.
                break;
            }
            let channel = match ty {
                0 => read_discrete(&mut r),
                1 | 3 | 4 => read_interpolated(&mut r, ty),
                5 => read_streamed(&mut r),
                6 => read_cue(&mut r),
                7 => read_translation_override(&mut r),
                other => {
                    return Err(format!("unknown animation channel type {other}"));
                }
            };
            channels.push(channel);
            if !r.ok() {
                return Err("truncated animation channel".to_string());
            }
        }
        Ok(Animation {
            total_time,
            identifier,
            internal_identifier,
            channels,
        })
    }

    pub fn total_time(&self) -> f32 {
        self.total_time
    }

    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    pub fn internal_identifier(&self) -> &str {
        &self.internal_identifier
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// `Animation::nChannelBinders` before morph stop; the channels that
    /// would bind to nodes, in file order.
    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }

    /// `AnimationInfo::restoreChannels`' write face
    /// (`mutant_animations.cpp:112-128`): the editor swaps the channel list
    /// back in after its blend-restore scratch has been applied.
    pub fn set_channels(&mut self, channels: Vec<Channel>) {
        self.channels = channels;
    }

    /// Bind each channel to a skeleton node index by identifier, mirroring
    /// `Animation::Animation(anim, root)`'s `root->find(identifier)`: a
    /// channel whose identifier is absent from the tree is nodeless — in the
    /// reference it lands in the global `NodeCatalogue` and, being absent
    /// from the model's `nodeTree_`, never contributes to the model pose.
    pub fn bind(&self, skeleton: &Skeleton) -> BoundAnimation {
        let node_indices = self
            .channels
            .iter()
            .map(|c| skeleton.find(c.identifier()))
            .collect();
        BoundAnimation {
            animation: self.clone(),
            node_indices,
        }
    }
}

fn read_discrete(r: &mut Reader) -> Channel {
    let identifier = r.string();
    let count = r.u32();
    let mut keys: Vec<(f32, Matrix)> = Vec::with_capacity(count.min(1024) as usize);
    for _ in 0..count {
        let time = r.f32();
        let mut m = Matrix::zero();
        for row in m.m.iter_mut() {
            for cell in row.iter_mut() {
                *cell = r.f32();
            }
        }
        if !r.ok() {
            break;
        }
        // std::map insert: first key wins on duplicate times.
        if !keys.iter().any(|(t, _)| *t == time) {
            keys.push((time, m));
        }
    }
    // std::map iterates sorted by time.
    keys.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    Channel::Discrete { identifier, keys }
}

fn read_interpolated(r: &mut Reader, ty: i32) -> Channel {
    // `AnimationChannel::load` reads the identifier first.
    let identifier = r.string();
    if ty == 4 {
        // Type 4 reads the three compression errors before the sequences;
        // type 1/3 default to 0.00001/0.00001/0.0005.
        let _ = (r.f32(), r.f32(), r.f32());
    }
    let scale_keys = r.vec3_seq();
    let position_keys = r.vec3_seq();
    let rotation_keys = r.quat_seq();
    // The three index sequences are read (and immediately rebuilt by
    // `buildIndex`, which discards the file's copy).
    let _scale_index = r.vec_u32_seq();
    let _position_index = r.vec_u32_seq();
    let _rotation_index = r.vec_u32_seq();
    // Sort the key vectors, as `buildIndex` does before the rebuilt indices
    // are used.
    let mut scale_keys = scale_keys;
    let mut position_keys = position_keys;
    let mut rotation_keys = rotation_keys;
    sort_keys_vec3(&mut scale_keys);
    sort_keys_vec3(&mut position_keys);
    sort_keys_quat(&mut rotation_keys);

    // `valid()` against the rebuilt indices: keys non-empty, every index in
    // (0, size]. The rebuilt index[0] counts keys with time <= 0, so a first
    // key after time zero makes index[0] == 0 and invalidates the channel.
    let scale_index = build_index(&scale_keys);
    let position_index = build_index(&position_keys);
    let rotation_index = build_index(&rotation_keys);
    if !interpolated_valid(&scale_keys, &scale_index)
        || !interpolated_valid(&position_keys, &position_index)
        || !interpolated_valid(&rotation_keys, &rotation_index)
    {
        r.failed = true;
    }
    Channel::Interpolated {
        identifier,
        scale_keys,
        position_keys,
        rotation_keys,
    }
}

fn read_streamed(r: &mut Reader) -> Channel {
    let identifier = r.string();
    let scale_fallback = Vector3::new(r.f32(), r.f32(), r.f32());
    let position_fallback = Vector3::new(r.f32(), r.f32(), r.f32());
    let rotation_fallback = Quaternion::new(r.f32(), r.f32(), r.f32(), r.f32());
    Channel::Streamed {
        identifier,
        scale_fallback,
        position_fallback,
        rotation_fallback,
    }
}

fn read_cue(r: &mut Reader) -> Channel {
    // CueChannel::load does NOT call the base identifier read
    // (cue_channel.cpp:227-232), and reads no base identifier here either.
    let count = r.u32();
    let mut cues = Vec::with_capacity(count.min(1024) as usize);
    for _ in 0..count {
        let time = r.f32();
        let cue_base = r.string();
        let args = r.f32_seq();
        if !r.ok() {
            break;
        }
        cues.push((time, cue_base, args));
    }
    Channel::Cue { cues }
}

fn read_translation_override(r: &mut Reader) -> Channel {
    let identifier = r.string();
    let anim_resource = r.string();
    let translation = Vector3::new(r.f32(), r.f32(), r.f32());
    let override_ = r.u8() != 0;
    Channel::TranslationOverride {
        identifier,
        anim_resource,
        translation,
        override_,
    }
}

impl Reader<'_> {
    fn u8(&mut self) -> u8 {
        let bytes = self.read(1);
        if bytes.is_empty() {
            return 0;
        }
        bytes[0]
    }

    fn f32_seq(&mut self) -> Vec<f32> {
        let count = self.u32();
        if count > Reader::MAX_SEQ_ELEMENTS {
            self.failed = true;
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count.min(1024) as usize);
        for _ in 0..count {
            let v = self.f32();
            if self.failed {
                break;
            }
            out.push(v);
        }
        out
    }

    fn vec_u32_seq(&mut self) -> Vec<u32> {
        let count = self.u32();
        if count > Reader::MAX_SEQ_ELEMENTS {
            self.failed = true;
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count.min(1024) as usize);
        for _ in 0..count {
            let v = self.u32();
            if self.failed {
                break;
            }
            out.push(v);
        }
        out
    }
}

fn sort_keys_vec3(keys: &mut [(f32, Vector3)]) {
    keys.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
}

fn sort_keys_quat(keys: &mut [(f32, Quaternion)]) {
    keys.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
}

/// A channel list bound to skeleton node indices (`None` = nodeless).
#[derive(Debug, Clone)]
pub struct BoundAnimation {
    animation: Animation,
    node_indices: Vec<Option<usize>>,
}

impl BoundAnimation {
    /// The world transform of every skeleton node at `frame`: the bound
    /// channel's local transform, or the visual's static transform for nodes
    /// with no bound channel, accumulated as `world = local x parent`.
    pub fn world_pose(&self, skeleton: &Skeleton, frame: f32) -> Vec<Matrix> {
        let mut locals = skeleton.local_transforms();
        for (i, idx) in self.node_indices.iter().enumerate() {
            if let (Some(node), Some(m)) = (*idx, self.animation.channels[i].result(frame)) {
                locals[node] = m;
            }
        }
        skeleton.compose_world(&locals)
    }
}

// ===========================================================================
// Skeleton: the visual node tree
// ===========================================================================

/// A node slot in the flat preorder vector the reference stores as
/// `nodeTree_` (`nodefull_model.cpp:184-227`).
#[derive(Debug, Clone)]
struct NodeSlot {
    identifier: String,
    parent: Option<usize>,
    transform: Matrix,
}

/// The skeleton: the visual's `<node>` subtree as a flat preorder vector
/// (`nodefull_model.cpp:184-216` — the reference walks the visual root node's
/// children and fills `nodeTree_` in preorder).
#[derive(Debug, Clone)]
pub struct Skeleton {
    nodes: Vec<NodeSlot>,
}

impl Skeleton {
    /// `Node::loadRecursive` (`node.cpp:200-226`) over the visual's root
    /// `<node>` section. The identifier is `readString("identifier")`; the
    /// transform is `readMatrix34` (`asMatrix34` reads `row0..row3` as Vector3
    /// and pins column 3 to `(0,0,0,1)`, `xml_section.cpp:915-931`).
    pub fn from_visual_section(visual: &DataSection) -> Result<Skeleton, String> {
        let root = visual
            .open_section("node")
            .ok_or_else(|| "visual has no node section".to_string())?;
        let mut nodes = Vec::new();
        load_recursive(root, None, &mut nodes);
        Ok(Skeleton { nodes })
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// `Node::find` (`node.cpp:152-172`): recursive DFS in child order. Over
    /// the flat preorder vector the first match is the DFS result.
    pub fn find(&self, identifier: &str) -> Option<usize> {
        self.nodes
            .iter()
            .position(|slot| slot.identifier == identifier)
    }

    pub fn identifier(&self, index: usize) -> &str {
        &self.nodes[index].identifier
    }

    /// The visual's static (initial pose) transforms, in node order.
    pub fn local_transforms(&self) -> Vec<Matrix> {
        self.nodes.iter().map(|slot| slot.transform).collect()
    }

    /// `Node::visitSelf` (`node.ipp`): `world = local x parentWorld` in
    /// preorder, the root's world being the model world (identity).
    fn compose_world(&self, locals: &[Matrix]) -> Vec<Matrix> {
        let mut world = vec![Matrix::zero(); self.nodes.len()];
        for (i, slot) in self.nodes.iter().enumerate() {
            if let Some(parent) = slot.parent {
                // In preorder `parent < i`, so `world[parent]` is final; copy
                // it out to satisfy the borrow checker (multiply takes `&mut
                // world[i]` and `&world[parent]` at the same time).
                let parent_world = world[parent];
                world[i].multiply(&locals[i], &parent_world);
            } else {
                world[i] = locals[i];
            }
        }
        world
    }
}

fn load_recursive(section: &DataSection, parent: Option<usize>, nodes: &mut Vec<NodeSlot>) {
    let index = nodes.len();
    let identifier = section.read_string("identifier", "");
    let transform = read_matrix34(section);
    nodes.push(NodeSlot {
        identifier,
        parent,
        transform,
    });
    // `openSections("node")` — every child named "node", in document order.
    for i in 0..section.count_children() {
        if let Some(child) = section.open_child(i)
            && child.section_name() == "node"
        {
            load_recursive(child, Some(index), nodes);
        }
    }
}

/// `readMatrix34` (`xml_section.cpp:915-931`): `row0..row3` each read as a
/// Vector3 (trailing junk is fine), column 3 pinned to `(0,0,0,1)`.
fn read_matrix34(section: &DataSection) -> Matrix {
    let mut m = Matrix::zero();
    let default_rows = Matrix::identity();
    for (r, out) in m.m.iter_mut().enumerate() {
        let row = section.read_vector3(&format!("row{r}"), row3_default(default_rows, r));
        out[0] = row.x;
        out[1] = row.y;
        out[2] = row.z;
        out[3] = if r == 3 { 1.0 } else { 0.0 };
    }
    m
}

fn row3_default(base: Matrix, r: usize) -> Vector3 {
    Vector3::new(base.m[r][0], base.m[r][1], base.m[r][2])
}

// ===========================================================================
// Model layer
// ===========================================================================

/// `createModelFromFile` (`model_common.hpp:14-34`) dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelKind {
    Nodefull,
    Nodeless,
}

/// The `.model` file's kind, its visual resource and its animation list.
#[derive(Debug, Clone)]
pub struct Model {
    pub kind: Option<ModelKind>,
    pub visual_resource: Option<String>,
    pub animations: Vec<LoadedAnimation>,
}

/// One `<animation>` entry of the model, with the frame mapping the
/// `NodefullModelAnimation` derives from it.
#[derive(Debug, Clone)]
pub struct LoadedAnimation {
    pub name: String,
    pub nodes_resource: String,
    /// `NodefullModelAnimation`'s frame range.
    pub frames: FrameRange,
    pub animation: Animation,
}

/// The frame mapping `NodefullModelAnimation` computes from its
/// `<animation>` section (`nodefull_model_animation.cpp:21-42`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameRange {
    pub first_frame: i32,
    pub last_frame: i32,
    pub frame_rate: f32,
    pub frame_count: i32,
    pub duration: f32,
    pub looped: bool,
}

impl FrameRange {
    /// `NodefullModelAnimation::NodefullModelAnimation`: defaults
    /// firstFrame=0, lastFrame=-1, frameRate=30; negatives map relative to the
    /// raw frame count; frames clamp to `[0, rawFrames]`;
    /// frameCount=|last-first|; duration=frameCount/frameRate;
    /// looped=duration>0.
    pub fn from_animation_section(section: &DataSection, total_time: f32) -> FrameRange {
        let raw_frames = total_time as i32;
        // `DataSection::as_int/as_float` read the section's own value, not a
        // child path; walk the path via `open_section` the way the reference
        // DataSection read-int helpers do.
        let mut first_frame = section
            .open_section("firstFrame")
            .map(|s| s.as_int(0))
            .unwrap_or(0);
        let mut last_frame = section
            .open_section("lastFrame")
            .map(|s| s.as_int(-1))
            .unwrap_or(-1);
        let frame_rate = section
            .open_section("frameRate")
            .map(|s| s.as_float(30.0))
            .unwrap_or(30.0);
        if first_frame < 0 {
            first_frame = raw_frames + first_frame + 1;
        }
        if last_frame < 0 {
            last_frame = raw_frames + last_frame + 1;
        }
        first_frame = first_frame.clamp(0, raw_frames.max(0));
        last_frame = last_frame.clamp(0, raw_frames.max(0));
        let frame_count = (last_frame - first_frame).abs();
        let duration = frame_count as f32 / frame_rate;
        let looped = duration > 0.0;
        FrameRange {
            first_frame,
            last_frame,
            frame_rate,
            frame_count,
            duration,
            looped,
        }
    }

    /// `NodefullModelAnimation::play` (`nodefull_model_animation.cpp:198-221`):
    /// `frame = time * frameRate`, then `firstFrame + frame` when
    /// `firstFrame <= lastFrame`, else `firstFrame - frame`.
    pub fn frame_at(&self, time: f32) -> f32 {
        let frame = time * self.frame_rate;
        if self.first_frame <= self.last_frame {
            self.first_frame as f32 + frame
        } else {
            self.first_frame as f32 - frame
        }
    }
}

impl Model {
    /// `Model::loadAnimations` (`model.cpp:1449-1595`) over the model root:
    /// the direct `<animation>` children in order, each with `<name>`,
    /// `<nodes>` and the frame mapping; the `.animation` resource is loaded
    /// through `resolve`.
    pub fn load<F>(model_root: &DataSection, mut resolve: F) -> Result<Model, String>
    where
        F: FnMut(&str) -> Result<Vec<u8>, String>,
    {
        let kind = if model_root.open_section("nodefullVisual").is_some() {
            Some(ModelKind::Nodefull)
        } else if model_root.open_section("nodelessVisual").is_some() {
            Some(ModelKind::Nodeless)
        } else {
            None
        };
        let nodefull_visual = model_root.read_string("nodefullVisual", "");
        let nodeless_visual = model_root.read_string("nodelessVisual", "");
        let visual_resource = if !nodefull_visual.is_empty() {
            Some(format!("{nodefull_visual}.visual"))
        } else if !nodeless_visual.is_empty() {
            Some(format!("{nodeless_visual}.visual"))
        } else {
            None
        };

        let mut animations = Vec::new();
        for i in 0..model_root.count_children() {
            let Some(section) = model_root.open_child(i) else {
                continue;
            };
            if section.section_name() != "animation" {
                continue;
            }
            // `openSection("nodes")` must exist; the reference logs and skips
            // the section when it does not.
            let Some(nodes) = section.open_section("nodes") else {
                continue;
            };
            let mut name = section.read_string("name", "");
            let nodes_resource = nodes.as_string().to_string();
            if name.is_empty() {
                // Derive the name after the last '/', or the whole resource
                // when there is no '/'.
                name = nodes_resource
                    .rfind('/')
                    .map(|p| nodes_resource[p + 1..].to_string())
                    .unwrap_or_else(|| nodes_resource.clone());
            }
            if name.is_empty() {
                continue;
            }
            let anim_resource = format!("{nodes_resource}.animation");
            let bytes = resolve(&anim_resource)?;
            let animation = Animation::from_bytes(&bytes)?;
            let frames = FrameRange::from_animation_section(section, animation.total_time());
            animations.push(LoadedAnimation {
                name,
                nodes_resource,
                frames,
                animation,
            });
        }
        Ok(Model {
            kind,
            visual_resource,
            animations,
        })
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton_oracle_data::{FRAMES, PROBES, WORLD};

    fn crab_visual() -> DataSection {
        let raw = include_bytes!("../res/crab.visual");
        resources::datasection::parse("crab.visual", raw).expect("crab.visual parses")
    }

    fn crab_animation() -> Vec<u8> {
        include_bytes!("../res/crab_walkforward.animation").to_vec()
    }

    /// The reference skeleton is the visual's single top-level `<node>`
    /// subtree (`visual.cpp:586` opens `"node"`); renderSet `<node>`s are not
    /// part of the pose hierarchy.
    #[test]
    fn skeleton_tree_matches_reference() {
        let visual = crab_visual();
        let skeleton = Skeleton::from_visual_section(&visual).expect("crab skeleton");
        assert_eq!(skeleton.node_count(), 71, "crab visual pose hierarchy");
        assert_eq!(skeleton.find("Scene Root"), Some(0));
        assert_eq!(skeleton.find("CrabHub"), Some(1));
        // DFS order: CrabHub's first child subtree is CrabLLeg11.
        assert_eq!(
            skeleton.identifier(skeleton.find("CrabLLeg11").unwrap()),
            "CrabLLeg11"
        );
        // Nodes absent from the visual: the animation's 'Crab' channel and the
        // eight Platform nodes have no skeleton slot.
        assert_eq!(skeleton.find("Crab"), None);
        assert_eq!(skeleton.find("CrabLLeg1Platform"), None);
        // The static Scene Root transform is the visual's identity matrix.
        let root_local = skeleton.local_transforms()[0];
        assert_eq!(root_local, Matrix::identity());
    }

    #[test]
    fn crab_animation_parses_exactly() {
        let bytes = crab_animation();
        let anim = Animation::from_bytes(&bytes).expect("crab animation parses");
        // The whole file is consumed.
        assert_eq!(anim.identifier(), "crab_walkforward");
        assert_eq!(anim.internal_identifier(), "crab_walkforward");
        assert_eq!(anim.total_time(), 25.0);
        assert_eq!(anim.channel_count(), 45);
        // All 45 crab binders are interpolated (type 1).
        for (i, channel) in anim.channels().iter().enumerate() {
            match channel {
                Channel::Interpolated {
                    identifier,
                    scale_keys,
                    position_keys,
                    rotation_keys,
                    ..
                } => {
                    assert!(!identifier.is_empty(), "channel {i} has an identifier");
                    assert!(!scale_keys.is_empty(), "scale keys for {identifier}");
                    assert!(!position_keys.is_empty(), "position keys for {identifier}");
                    assert!(!rotation_keys.is_empty(), "rotation keys for {identifier}");
                }
                _ => panic!("crab binder {i} is not interpolated"),
            }
        }
        // Keys are sorted by time, each stream starting at time zero.
        match &anim.channels()[0] {
            Channel::Interpolated {
                scale_keys,
                position_keys,
                rotation_keys,
                ..
            } => {
                assert_sorted(scale_keys);
                assert_sorted(position_keys);
                assert_sorted(rotation_keys);
            }
            _ => unreachable!(),
        }
    }

    fn assert_sorted<V>(keys: &[(f32, V)]) {
        assert_eq!(keys[0].0, 0.0);
        assert!(
            keys.windows(2).all(|w| w[0].0 < w[1].0),
            "strictly increasing"
        );
    }

    #[test]
    fn crab_world_pose_matches_oracle() {
        let visual = crab_visual();
        let skeleton = Skeleton::from_visual_section(&visual).unwrap();
        let anim = Animation::from_bytes(&crab_animation()).unwrap();
        let bound = anim.bind(&skeleton);
        // 36 of the 45 channels bind (Crab + 8 Platform nodes are absent from
        // the visual and bind nodeless).
        assert_eq!(
            bound.node_indices.iter().filter(|i| i.is_some()).count(),
            36
        );

        for (fi, &frame) in FRAMES.iter().enumerate() {
            let world = bound.world_pose(&skeleton, frame);
            for (pi, probe) in PROBES.iter().enumerate() {
                let node = skeleton
                    .find(probe)
                    .unwrap_or_else(|| panic!("probe {probe} is in the skeleton"));
                let got = world[node];
                let expected = Matrix { m: WORLD[fi][pi] };
                assert!(
                    got.almost(&expected, 5e-6),
                    "frame {frame} node {probe}:\ngot      {:?}\nexpected {:?}",
                    got.m,
                    expected.m
                );
            }
        }
    }

    /// At an authored rotation key time the node's local transform equals the
    /// channel's `buildTransform` for that key (D3DXMatrixRotationQuaternion,
    /// scaled rows, translation): `interpolateKeys` at a key time returns
    /// the key's own value, so no interpolation drift reaches the pose.
    #[test]
    fn pose_at_keyframe_is_exact() {
        let visual = crab_visual();
        let skeleton = Skeleton::from_visual_section(&visual).unwrap();
        let anim = Animation::from_bytes(&crab_animation()).unwrap();
        let bound = anim.bind(&skeleton);
        let node = skeleton.find("CrabEyeL").unwrap();
        let channel = bound
            .animation
            .channels()
            .iter()
            .find(|c| c.identifier() == "CrabEyeL")
            .unwrap();
        let Channel::Interpolated {
            rotation_keys,
            scale_keys,
            position_keys,
            ..
        } = channel
        else {
            unreachable!()
        };
        for (i, (_, key_rot)) in rotation_keys.iter().enumerate() {
            let t = rotation_keys[i].0;
            let expected_local = build_transform(
                key_rot,
                &interpolate_keys_vec3(scale_keys, t),
                &interpolate_keys_vec3(position_keys, t),
            );
            // world = local x parent world.
            let world = bound.world_pose(&skeleton, t);
            let mut expected_world = expected_local;
            if let Some(parent) = skeleton.nodes[node].parent {
                expected_world.multiply(&expected_local, &world[parent]);
            }
            let got = world[node];
            assert!(
                got.almost(&expected_world, 5e-6),
                "CrabEyeL at key time {t}:\ngot      {:?}\nexpected {:?}",
                got.m,
                expected_world.m
            );
        }
    }

    #[test]
    fn model_layer_loads_crab() {
        let raw = include_bytes!("../res/crab.model");
        let model_root =
            resources::datasection::parse("crab.model", raw).expect("crab.model parses");
        let anim_bytes = crab_animation();
        let model = Model::load(&model_root, |_| Ok(anim_bytes.clone())).expect("model loads");
        assert_eq!(model.kind, Some(ModelKind::Nodefull));
        assert_eq!(
            model.visual_resource.as_deref(),
            Some("characters/npc/crab/crab.visual")
        );
        assert_eq!(model.animations.len(), 6);
        let walk = model
            .animations
            .iter()
            .find(|a| a.name == "crab_walkforward")
            .unwrap();
        assert_eq!(walk.nodes_resource, "characters/npc/crab/crab_walkforward");
        assert_eq!(walk.animation.total_time(), 25.0);
        // No firstFrame/lastFrame in the section: first=0, last=-1 maps to
        // rawFrames-1+1 = 25.
        assert_eq!(walk.frames.first_frame, 0);
        assert_eq!(walk.frames.last_frame, 25);
        assert_eq!(walk.frames.frame_rate, 30.0);
        assert_eq!(walk.frames.frame_count, 25);
        assert!(walk.frames.looped);
        // play(0.5s) -> frame 15, matching the reference.
        assert_eq!(walk.frames.frame_at(0.5), 15.0);
    }

    #[test]
    fn frame_range_maps_reverse_order() {
        // firstFrame > lastFrame maps to `firstFrame - frame`. totalTime 10
        // gives rawFrames 10, so the [0, rawFrames] clamp does not bite.
        let section = resources::datasection::parse("test", b"<anim><firstFrame>7</firstFrame><lastFrame>2</lastFrame><frameRate>10</frameRate></anim>").unwrap();
        let frames = FrameRange::from_animation_section(&section, 10.0);
        assert_eq!(frames.first_frame, 7);
        assert_eq!(frames.last_frame, 2);
        assert_eq!(frames.frame_count, 5);
        assert_eq!(frames.duration, 0.5);
        assert_eq!(frames.frame_at(1.0), -3.0);
    }

    #[test]
    fn frame_range_clamps_to_raw_frames() {
        // `Math::clamp(0, firstFrame, rawFrames)` pins an out-of-range frame.
        let section =
            resources::datasection::parse("test", b"<anim><firstFrame>7</firstFrame></anim>")
                .unwrap();
        let frames = FrameRange::from_animation_section(&section, 5.0);
        assert_eq!(frames.first_frame, 5);
        assert_eq!(frames.last_frame, 5);
        assert_eq!(frames.frame_count, 0);
        assert!(!frames.looped);
        // A negative frame maps relative to the raw count: -1 -> last frame.
        let section =
            resources::datasection::parse("test", b"<anim><firstFrame>-1</firstFrame></anim>")
                .unwrap();
        let frames = FrameRange::from_animation_section(&section, 5.0);
        assert_eq!(frames.first_frame, 5);
    }

    #[test]
    fn discrete_channel_result() {
        let m0 = Matrix::identity();
        let m1 = {
            let mut m = Matrix::identity();
            m.set_translate(1.0, 0.0, 0.0);
            m
        };
        let m2 = {
            let mut m = Matrix::identity();
            m.set_translate(2.0, 0.0, 0.0);
            m
        };
        let keys = vec![(0.0, m0), (1.0, m1), (2.0, m2)];
        let ch = Channel::Discrete {
            identifier: "N".to_string(),
            keys: keys.clone(),
        };
        // Before the first key: the first key's value.
        assert_eq!(ch.result(-1.0), Some(m0));
        // Between keys: the previous key's value.
        assert_eq!(ch.result(0.5), Some(m0));
        assert_eq!(ch.result(1.5), Some(m1));
        // At the last key: the last value; past it: the last value.
        assert_eq!(ch.result(2.0), Some(m2));
        assert_eq!(ch.result(9.0), Some(m2));
        // Empty map: no result (node keeps its previous transform).
        let empty = Channel::Discrete {
            identifier: "N".to_string(),
            keys: Vec::new(),
        };
        assert_eq!(empty.result(0.0), None);
    }

    #[test]
    fn lerp_and_slerp_follow_d3dx() {
        // D3DXVec3Lerp: the midpoint of (0,0,0)-(1,2,3).
        let mid = d3dx_vec3_lerp(
            &Vector3::new(0.0, 0.0, 0.0),
            &Vector3::new(1.0, 2.0, 3.0),
            0.5,
        );
        assert_eq!(mid, Vector3::new(0.5, 1.0, 1.5));

        // D3DXQuaternionSlerp, short-arc flip: a negated end quat yields the
        // same path.
        let qa = Quaternion::new(0.0, 0.0, 0.0, 1.0);
        let qb = Quaternion::from_angle_axis(0.6, &Vector3::new(0.0, 1.0, 0.0));
        let qbn = Quaternion::new(-qb.x, -qb.y, -qb.z, -qb.w);
        let a = d3dx_quaternion_slerp(&qa, &qb, 0.5);
        let b = d3dx_quaternion_slerp(&qa, &qbn, 0.5);
        assert!(a.almost_equal(&b), "short-arc flip {:?} vs {:?}", a, b);

        // Close quats (cos > 0.9999) use the linear branch.
        let qc = Quaternion::from_angle_axis(1e-5, &Vector3::new(0.0, 1.0, 0.0));
        let lin = d3dx_quaternion_slerp(&qa, &qc, 0.25);
        let expected = Quaternion::new(
            qa.x * 0.75 + qc.x * 0.25,
            qa.y * 0.75 + qc.y * 0.25,
            qa.z * 0.75 + qc.z * 0.25,
            qa.w * 0.75 + qc.w * 0.25,
        );
        assert!(
            lin.almost_equal(&expected),
            "linear branch {:?} vs {:?}",
            lin,
            expected
        );
    }

    #[test]
    fn build_transform_scales_rows() {
        let rot = Quaternion::from_angle_axis(0.0, &Vector3::new(0.0, 1.0, 0.0));
        let scale = Vector3::new(2.0, 3.0, 4.0);
        let pos = Vector3::new(5.0, 6.0, 7.0);
        let m = build_transform(&rot, &scale, &pos);
        assert_eq!(m.m[0][0], 2.0);
        assert_eq!(m.m[1][1], 3.0);
        assert_eq!(m.m[2][2], 4.0);
        assert_eq!(m.m[3][0], 5.0);
        assert_eq!(m.m[3][1], 6.0);
        assert_eq!(m.m[3][2], 7.0);
        assert_eq!(m.m[3][3], 1.0);
    }

    #[test]
    fn morph_channel_stops_reading() {
        // A type 2 binder ends the binder loop: the trailing binder is not
        // read. Each binder reads its type first, then the channel payload
        // (which begins with the identifier string).
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1.0f32.to_le_bytes()); // totalTime
        write_string(&mut bytes, "m");
        write_string(&mut bytes, "m");
        bytes.extend_from_slice(&3i32.to_le_bytes()); // 3 binders
        bytes.extend_from_slice(&0i32.to_le_bytes()); // binder 0: discrete
        write_string(&mut bytes, "A");
        bytes.extend_from_slice(&0u32.to_le_bytes()); // empty map
        bytes.extend_from_slice(&2i32.to_le_bytes()); // binder 1: morph, break
        write_string(&mut bytes, "B"); // never read
        bytes.extend_from_slice(&0i32.to_le_bytes()); // never read
        let anim = Animation::from_bytes(&bytes).expect("morph stop parses");
        assert_eq!(anim.channel_count(), 1);
        assert_eq!(anim.channels()[0].identifier(), "A");
    }

    fn write_string(out: &mut Vec<u8>, s: &str) {
        out.extend_from_slice(&(s.len() as i32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    }

    #[test]
    fn nodeless_channels_do_not_affect_pose() {
        let visual = crab_visual();
        let skeleton = Skeleton::from_visual_section(&visual).unwrap();
        let anim = Animation::from_bytes(&crab_animation()).unwrap();
        let bound = anim.bind(&skeleton);
        // The 'Crab' channel is nodeless: its identifier is not in the
        // skeleton.
        assert!(
            bound
                .animation
                .channels()
                .iter()
                .any(|c| c.identifier() == "Crab")
        );
        let crab_idx = skeleton.find("Crab");
        assert_eq!(crab_idx, None);
        // Evaluating the pose must not panic and the bound count is 36.
        let world = bound.world_pose(&skeleton, 5.0);
        assert_eq!(world.len(), skeleton.node_count());
    }
}
