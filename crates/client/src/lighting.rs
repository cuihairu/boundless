//! BW-11-04-03 lighting: the Moo light types and their container
//! (`lib/moo/directional_light.cpp`, `omni_light.cpp`, `spot_light.cpp`,
//! `pulse_light.cpp`, `light_container.cpp`, `lib/math/linear_animation.hpp`).
//!
//! The reference holds lights through `SmartPointer`s and dedups by pointer
//! identity; the port hands out `Rc<RefCell<..>>` handles and dedups with
//! `Rc::ptr_eq`. `PulseLight` extends `OmniLight` in the reference — the port
//! composes: it owns an omni handle (shared with any container the light was
//! added to) and `tick` writes straight through it, so container consumers
//! see the animated position and colour.
//!
//! Registered differences:
//! - `SpotLight::intersects` / `worldBounds` mutate cached bounds from a
//!   `const` method in the reference (`mutable` fields + `const_cast`); the
//!   port takes `&mut self`.
//! - `BoundBox::calculateOutcode` stored state is replaced by the pure
//!   helper `box_outside_clip_space` (same math: `boundbox.cpp:15-57` +
//!   `vector4.ipp:199-222`).
//! - The D3D-facing surfaces (`commitToFixedFunctionPipeline`,
//!   `addVertexShaderConstant`, `addExtraOmnisIn*Space`'s shader-constant
//!   writes, `getTerrainLight`'s GPU arrays) are out of scope here; the port
//!   keeps the light state they would have uploaded.
//! - The `addLight` overloads are literal aliases of `addOmni`/`addSpot`/
//!   `addDirectional` and are not duplicated.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::skeleton;
use foundation::math::boundbox::BoundBox;
use foundation::math::matrix::Matrix;
use foundation::math::plane::PlaneEq;
use foundation::math::vector::{Vector3, almost_zero};

// ===========================================================================
// Colour
// ===========================================================================

/// `Moo::Colour` = `D3DXCOLOR`: four float channels (`moo_math.hpp:27`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colour {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Colour {
    pub fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Colour { r, g, b, a }
    }
}

impl Default for Colour {
    fn default() -> Self {
        Colour::new(0.0, 0.0, 0.0, 0.0)
    }
}

// ===========================================================================
// DirectionalLight
// ===========================================================================

/// `DirectionalLight` (`directional_light.hpp:23-56`): a colour and a
/// direction, with the world-transformed direction cached.
#[derive(Debug, Clone)]
pub struct DirectionalLight {
    direction: Vector3,
    world_direction: Vector3,
    colour: Colour,
}

impl DirectionalLight {
    /// The default constructor (`directional_light.cpp:18-28`): black colour,
    /// direction (0, 0, 1), mirrored into world space.
    pub fn new() -> Self {
        DirectionalLight {
            direction: Vector3::new(0.0, 0.0, 1.0),
            world_direction: Vector3::new(0.0, 0.0, 1.0),
            colour: Colour::default(),
        }
    }

    /// `DirectionalLight(colour, direction)` (`directional_light.cpp:31-41`).
    pub fn with(colour: Colour, direction: Vector3) -> Self {
        DirectionalLight {
            direction,
            world_direction: direction,
            colour,
        }
    }

    pub fn direction(&self) -> &Vector3 {
        &self.direction
    }

    pub fn set_direction(&mut self, direction: Vector3) {
        self.direction = direction;
    }

    pub fn colour(&self) -> &Colour {
        &self.colour
    }

    pub fn set_colour(&mut self, colour: Colour) {
        self.colour = colour;
    }

    pub fn world_direction(&self) -> &Vector3 {
        &self.world_direction
    }

    /// `worldTransform` (`directional_light.cpp:61-79`): the direction passes
    /// through the inverse-transpose of the transform — the general inverse
    /// first, falling back to the raw transform when it is singular — then a
    /// normalise. Unlike `OmniLight`, non-uniform scale is allowed here.
    pub fn world_transform(&mut self, transform: &Matrix) {
        let mut t = *transform;
        if t.invert() {
            t.transpose();
        } else {
            t = *transform;
        }

        self.world_direction = t.apply_vector(&self.direction);
        self.world_direction.normalise();
    }
}

impl Default for DirectionalLight {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// OmniLight
// ===========================================================================

/// `OmniLight` (`omni_light.hpp:25-100`): colour, position and an attenuation
/// range, with the world-transformed state cached.
#[derive(Debug, Clone)]
pub struct OmniLight {
    world_bounds: BoundBox,
    position: Vector3,
    inner_radius: f32,
    outer_radius: f32,
    colour: Colour,
    world_position: Vector3,
    world_inner_radius: f32,
    world_outer_radius: f32,
    priority: i32,
}

impl OmniLight {
    /// The default constructor (`omni_light.cpp:18-37`): position (0,0,0),
    /// radii 0, black colour, priority 0, world state mirrored.
    pub fn new() -> Self {
        OmniLight {
            world_bounds: BoundBox::default(),
            position: Vector3::new(0.0, 0.0, 0.0),
            inner_radius: 0.0,
            outer_radius: 0.0,
            colour: Colour::default(),
            world_position: Vector3::new(0.0, 0.0, 0.0),
            world_inner_radius: 0.0,
            world_outer_radius: 0.0,
            priority: 0,
        }
    }

    /// `OmniLight(colour, position, innerRadius, outerRadius)`
    /// (`omni_light.cpp:40-56`).
    pub fn with(colour: Colour, position: Vector3, inner_radius: f32, outer_radius: f32) -> Self {
        OmniLight {
            world_bounds: BoundBox::default(),
            position,
            inner_radius,
            outer_radius,
            colour,
            world_position: position,
            world_inner_radius: inner_radius,
            world_outer_radius: outer_radius,
            priority: 0,
        }
    }

    pub fn position(&self) -> &Vector3 {
        &self.position
    }

    pub fn set_position(&mut self, position: Vector3) {
        self.position = position;
    }

    /// `innerRadius` (`omni_light.ipp:45-52`): raising inner past outer
    /// drags outer along.
    pub fn set_inner_radius(&mut self, inner_radius: f32) {
        self.inner_radius = inner_radius;
        if self.inner_radius > self.outer_radius {
            self.set_outer_radius(inner_radius);
        }
    }

    pub fn inner_radius(&self) -> f32 {
        self.inner_radius
    }

    /// `outerRadius` (`omni_light.ipp:62-69`): shrinking outer below inner
    /// drags inner down.
    pub fn set_outer_radius(&mut self, outer_radius: f32) {
        self.outer_radius = outer_radius;
        if self.inner_radius > self.outer_radius {
            self.set_inner_radius(outer_radius);
        }
    }

    pub fn outer_radius(&self) -> f32 {
        self.outer_radius
    }

    pub fn colour(&self) -> &Colour {
        &self.colour
    }

    pub fn set_colour(&mut self, colour: Colour) {
        self.colour = colour;
    }

    pub fn priority(&self) -> i32 {
        self.priority
    }

    pub fn set_priority(&mut self, priority: i32) {
        self.priority = priority;
    }

    pub fn world_position(&self) -> &Vector3 {
        &self.world_position
    }

    pub fn world_inner_radius(&self) -> f32 {
        self.world_inner_radius
    }

    pub fn world_outer_radius(&self) -> f32 {
        self.world_outer_radius
    }

    pub fn world_bounds(&self) -> &BoundBox {
        &self.world_bounds
    }

    /// `worldTransform` (`omni_light.cpp:70-96`): the position transforms as
    /// a point, the radii scale uniformly (the reference `MF_ASSERT`s a
    /// uniform scale) and the world bounds are the local sphere box run
    /// through the transform.
    pub fn world_transform(&mut self, transform: &Matrix) {
        self.world_position = transform.apply_point(&self.position);

        assert!(
            transform.is_uniformly_scaled(),
            "OmniLight::worldTransform requires a uniformly scaled transform"
        );
        let scale = transform.uniform_scale();

        self.world_inner_radius = self.inner_radius * scale;
        self.world_outer_radius = self.outer_radius * scale;

        let expand = Vector3::new(self.outer_radius, self.outer_radius, self.outer_radius);
        let mut local_bounds = BoundBox::new(self.position - expand, self.position + expand);
        local_bounds.transform_by(transform);
        self.world_bounds = local_bounds;
    }

    /// `intersects` (`omni_light.ipp:115-118`): the world-space box probes
    /// the outer-radius sphere (the second reference parameter is a bias).
    pub fn intersects(&self, world_space_bb: &BoundBox) -> bool {
        world_space_bb.intersects_point(&self.world_position, self.world_outer_radius)
    }

    /// `attenuation` (`omni_light.cpp:113-130`): 2 at the centre, 1 at the
    /// inner radius, 0 at the outer radius and beyond, linear in between.
    pub fn attenuation(&self, world_space_bb: &BoundBox) -> f32 {
        let distance = world_space_bb.distance(&self.world_position);

        let mut att = 0.0;
        if distance < self.world_inner_radius {
            att = 2.0 - distance / self.world_inner_radius;
        } else if distance < self.world_outer_radius {
            att = (distance - self.world_outer_radius)
                / (self.world_inner_radius - self.world_outer_radius);
        }
        att
    }
}

impl Default for OmniLight {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// SpotLight
// ===========================================================================

/// `SpotLight` (`spot_light.hpp:25-112`): an omni cone with a direction and a
/// cone angle, caching the cone's bounds and light view for culling.
#[derive(Debug, Clone)]
pub struct SpotLight {
    dirty: bool,
    position: Vector3,
    direction: Vector3,
    inner_radius: f32,
    outer_radius: f32,
    cos_cone_angle: f32,
    colour: Colour,
    world_position: Vector3,
    world_direction: Vector3,
    world_inner_radius: f32,
    world_outer_radius: f32,
    priority: i32,
    light_view: Matrix,
    light_bounds: BoundBox,
}

impl SpotLight {
    /// The default constructor (`spot_light.cpp:19-42`): direction (0,0,1),
    /// cos cone angle 1 (a zero-degree cone), dirty bounds.
    pub fn new() -> Self {
        SpotLight {
            dirty: true,
            position: Vector3::new(0.0, 0.0, 0.0),
            direction: Vector3::new(0.0, 0.0, 1.0),
            inner_radius: 0.0,
            outer_radius: 0.0,
            cos_cone_angle: 1.0,
            colour: Colour::default(),
            world_position: Vector3::new(0.0, 0.0, 0.0),
            world_direction: Vector3::new(0.0, 0.0, 1.0),
            world_inner_radius: 0.0,
            world_outer_radius: 0.0,
            priority: 0,
            light_view: Matrix::zero(),
            light_bounds: BoundBox::default(),
        }
    }

    /// `SpotLight(colour, position, direction, innerRadius, outerRadius,
    /// cosConeAngle)` (`spot_light.cpp:45-66`).
    pub fn with(
        colour: Colour,
        position: Vector3,
        direction: Vector3,
        inner_radius: f32,
        outer_radius: f32,
        cos_cone_angle: f32,
    ) -> Self {
        SpotLight {
            dirty: true,
            position,
            direction,
            inner_radius,
            outer_radius,
            cos_cone_angle,
            colour,
            world_position: position,
            world_direction: direction,
            world_inner_radius: inner_radius,
            world_outer_radius: outer_radius,
            priority: 0,
            light_view: Matrix::zero(),
            light_bounds: BoundBox::default(),
        }
    }

    pub fn position(&self) -> &Vector3 {
        &self.position
    }

    pub fn set_position(&mut self, position: Vector3) {
        self.position = position;
    }

    pub fn direction(&self) -> &Vector3 {
        &self.direction
    }

    pub fn set_direction(&mut self, direction: Vector3) {
        self.dirty = true;
        self.direction = direction;
    }

    /// `innerRadius` (`spot_light.ipp:36-44`), with the same mutual clamp as
    /// the omni.
    pub fn set_inner_radius(&mut self, inner_radius: f32) {
        self.inner_radius = inner_radius;
        if self.inner_radius > self.outer_radius {
            self.set_outer_radius(inner_radius);
        }
    }

    pub fn inner_radius(&self) -> f32 {
        self.inner_radius
    }

    pub fn set_outer_radius(&mut self, outer_radius: f32) {
        self.dirty = true;
        self.outer_radius = outer_radius;
        if self.inner_radius > self.outer_radius {
            self.set_inner_radius(outer_radius);
        }
    }

    pub fn outer_radius(&self) -> f32 {
        self.outer_radius
    }

    pub fn cos_cone_angle(&self) -> f32 {
        self.cos_cone_angle
    }

    pub fn set_cos_cone_angle(&mut self, cos_cone_angle: f32) {
        self.dirty = true;
        self.cos_cone_angle = cos_cone_angle;
    }

    pub fn colour(&self) -> &Colour {
        &self.colour
    }

    pub fn set_colour(&mut self, colour: Colour) {
        self.colour = colour;
    }

    pub fn priority(&self) -> i32 {
        self.priority
    }

    pub fn set_priority(&mut self, priority: i32) {
        self.priority = priority;
    }

    pub fn world_position(&self) -> &Vector3 {
        &self.world_position
    }

    pub fn world_direction(&self) -> &Vector3 {
        &self.world_direction
    }

    pub fn world_inner_radius(&self) -> f32 {
        self.world_inner_radius
    }

    pub fn world_outer_radius(&self) -> f32 {
        self.world_outer_radius
    }

    /// `worldBounds` (`spot_light.ipp:106-111`): refreshes the cone bounds
    /// first (the reference caches them in `mutable` fields).
    pub fn world_bounds(&mut self) -> &BoundBox {
        self.update_internal_bounds();
        &self.light_bounds
    }

    /// `worldTransform` (`spot_light.cpp:71-100`): like the omni, plus the
    /// direction through the inverse-transpose and a dirty flag.
    pub fn world_transform(&mut self, transform: &Matrix) {
        self.world_position = transform.apply_point(&self.position);

        assert!(
            transform.is_uniformly_scaled(),
            "SpotLight::worldTransform requires a uniformly scaled transform"
        );
        let scale = transform.uniform_scale();

        self.world_inner_radius = self.inner_radius * scale;
        self.world_outer_radius = self.outer_radius * scale;

        let mut trans_normal = *transform;
        if trans_normal.invert() {
            trans_normal.transpose();
        }
        self.world_direction = trans_normal.apply_vector(&self.direction);
        self.world_direction.normalise();
        self.dirty = true;
    }

    /// `updateInternalBounds` (`spot_light.cpp:137-172`): the cone's bounding
    /// box (position to the cap circle's four extreme points) and the light
    /// view-projection used for the final frustum test.
    pub fn update_internal_bounds(&mut self) {
        if !self.dirty {
            return;
        }

        let angle = self.cos_cone_angle.acos();

        // Build bounding box for the view frustum
        self.light_bounds
            .set_bounds(self.world_position, self.world_position);

        let cap_centre = self.world_position + self.direction * self.outer_radius;
        let cap_radius = self.outer_radius * angle.tan();

        let cap_plane = PlaneEq::new(self.direction, self.outer_radius);
        let (xdir, ydir) = cap_plane.basis();

        self.light_bounds
            .add_bounds(&(cap_centre + xdir * cap_radius));
        self.light_bounds
            .add_bounds(&(cap_centre - xdir * cap_radius));
        self.light_bounds
            .add_bounds(&(cap_centre + ydir * cap_radius));
        self.light_bounds
            .add_bounds(&(cap_centre - ydir * cap_radius));

        // Build light view projection (the engine `Matrix::lookAt`, basis
        // columns — not the D3DX recipe).
        let mut lookat = Matrix::zero();
        let up = Vector3::new(0.0, 1.0, 0.0);
        lookat.look_at(&self.world_position, &self.direction, &up);
        let mut proj = Matrix::zero();
        proj.perspective_projection(angle * 2.0, 1.0, 0.0, self.outer_radius);

        self.light_view.multiply(&lookat, &proj);

        self.dirty = false;
    }

    /// `intersects` (`spot_light.ipp:135-154`): three gates — the outer
    /// sphere, the cone's bounding box, then the frustum outcode.
    pub fn intersects(&mut self, world_space_bb: &BoundBox) -> bool {
        if !world_space_bb.intersects_point(&self.world_position, self.world_outer_radius) {
            return false;
        }

        self.update_internal_bounds();
        if !world_space_bb.intersects(&self.light_bounds) {
            return false;
        }

        if box_outside_clip_space(world_space_bb, &self.light_view) {
            return false;
        }

        true
    }

    /// `attenuation` (`spot_light.cpp:117-135`): the omni ramp on the world
    /// radii.
    pub fn attenuation(&self, world_space_bb: &BoundBox) -> f32 {
        let distance = world_space_bb.distance(&self.world_position);

        let mut att = 0.0;
        if distance < self.world_inner_radius {
            att = 2.0 - distance / self.world_inner_radius;
        } else if distance < self.world_outer_radius {
            att = (distance - self.world_outer_radius)
                / (self.world_inner_radius - self.world_outer_radius);
        }
        att
    }
}

impl Default for SpotLight {
    fn default() -> Self {
        Self::new()
    }
}

/// The clip-space rejection test of `AABB::calculateOutcode`
/// (`boundbox.cpp:15-57`) with `Vector4::calculateOutcode`'s plane tests
/// (`vector4.ipp:199-222`): true when all eight transformed corners share at
/// least one clip-plane rejection (a nonzero combined outcode).
fn box_outside_clip_space(bb: &BoundBox, m: &Matrix) -> bool {
    let mut combined = 0b111111u32;

    let vx = [m.row(0) * bb.min.x, m.row(0) * bb.max.x];
    let vy = [m.row(1) * bb.min.y, m.row(1) * bb.max.y];
    let vz = [m.row(2) * bb.min.z, m.row(2) * bb.max.z];
    let vw = m.row(3);

    for i in 0..8 {
        let v = vw + vx[i & 1] + vy[(i >> 1) & 1] + vz[(i >> 2) & 1];

        let mut outcode = 0u32;
        if v.x < -v.w {
            outcode |= 1; // OUTCODE_LEFT
        }
        if v.x > v.w {
            outcode |= 2; // OUTCODE_RIGHT
        }
        if v.y < -v.w {
            outcode |= 4; // OUTCODE_BOTTOM
        }
        if v.y > v.w {
            outcode |= 8; // OUTCODE_TOP
        }
        if v.z < 0.0 {
            outcode |= 16; // OUTCODE_NEAR
        }
        if v.z > v.w {
            outcode |= 32; // OUTCODE_FAR
        }

        combined &= outcode;
    }
    combined != 0
}

// ===========================================================================
// LinearAnimation (the float instance the pulse light uses, and the vector
// instance the day/night cycle uses)
// ===========================================================================

/// A `std::map<float, Value>` key: `f32` ordered by `total_cmp` so the map
/// stays total (key times are finite in practice).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key(pub f32);

impl Eq for Key {}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// A value a [`LinearAnimation`] can hold and interpolate. The reference
/// template only ever default-constructs and mixes its values, which is all
/// this trait requires.
pub trait Interpolable: Copy {
    /// `Value(0)`: what `animate` returns for an animation with no keys.
    fn zero() -> Self;
    /// The value `fraction` of the way from `first` to `last`.
    fn mix(first: Self, last: Self, fraction: f32) -> Self;
}

impl Interpolable for f32 {
    fn zero() -> Self {
        0.0
    }

    fn mix(first: Self, last: Self, fraction: f32) -> Self {
        first + (last - first) * fraction
    }
}

impl Interpolable for Vector3 {
    fn zero() -> Self {
        Vector3::ZERO
    }

    fn mix(first: Self, last: Self, fraction: f32) -> Self {
        Vector3::new(
            first.x + (last.x - first.x) * fraction,
            first.y + (last.y - first.y) * fraction,
            first.z + (last.z - first.z) * fraction,
        )
    }
}

/// `LinearAnimation<Value>` (`lib/math/linear_animation.hpp`): a time-keyed
/// value map with optional looping and linear interpolation. The pulse light
/// instantiates it with `float`, the day/night cycle with `Vector3`.
#[derive(Debug, Clone, Default)]
pub struct LinearAnimation<V: Interpolable = f32> {
    keys: BTreeMap<Key, V>,
    loop_: bool,
    max_time: f32,
}

impl<V: Interpolable> LinearAnimation<V> {
    /// `LinearAnimation(loop, maxTime)`.
    pub fn new(loop_: bool, max_time: f32) -> Self {
        LinearAnimation {
            keys: BTreeMap::new(),
            loop_,
            max_time,
        }
    }

    /// `empty` (`linear_animation.hpp`): true when no key was added.
    pub fn empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// `clear` (the `std::map` base): drops every key, keeps loop/maxTime.
    pub fn clear(&mut self) {
        self.keys.clear();
    }

    /// `animate` (`linear_animation.hpp:20-101`): the value at `time`,
    /// interpolating between the neighbouring keys (wrapping through the loop
    /// seam when looping).
    pub fn animate(&self, time: f32) -> V {
        if self.keys.is_empty() {
            return V::zero();
        }
        if self.keys.len() == 1 {
            return *self.keys.values().next().unwrap();
        }

        if self.loop_ {
            let mut time = time;
            if time > self.max_time || time < 0.0 {
                time -= (time / self.max_time).floor() * self.max_time;
            }

            // upper_bound for the later key, the map entry before it for the
            // earlier one, each wrapping around the ends.
            let (first_time, first_value) = match self.keys.range(..=Key(time)).next_back() {
                Some((k, &v)) => (k.0, v),
                None => {
                    let (&k, &v) = self.keys.iter().next_back().unwrap();
                    (k.0, v)
                }
            };
            let (mut last_time, last_value) = match self
                .keys
                .range((
                    std::ops::Bound::Excluded(Key(time)),
                    std::ops::Bound::Unbounded,
                ))
                .next()
            {
                Some((k, &v)) => (k.0, v),
                None => {
                    let (&k, &v) = self.keys.iter().next().unwrap();
                    (k.0, v)
                }
            };

            if first_time == time {
                return first_value;
            }

            if first_time > last_time {
                last_time += self.max_time;
            }
            if first_time > time {
                time += self.max_time;
            }

            let fraction = (time - first_time) / (last_time - first_time);
            V::mix(first_value, last_value, fraction)
        } else {
            let upper = self
                .keys
                .range((
                    std::ops::Bound::Excluded(Key(time)),
                    std::ops::Bound::Unbounded,
                ))
                .next();

            let (last_time, last_value) = match upper {
                Some((k, &v)) => (k.0, v),
                // Past the last keyframe: return the last key's value.
                None => return *self.keys.values().next_back().unwrap(),
            };

            // Before the first keyframe: return the first key's value.
            let (first_time, first_value) = match self.keys.range(..=Key(time)).next_back() {
                Some((k, &v)) => (k.0, v),
                None => return last_value,
            };

            if time == first_time {
                return first_value;
            }

            let fraction = (time - first_time) / (last_time - first_time);
            V::mix(first_value, last_value, fraction)
        }
    }

    /// `addKey` (`linear_animation.hpp:103-120`): while looping, only keys
    /// inside `[0, maxTime)` are accepted; `operator[]` overwrite semantics.
    pub fn add_key(&mut self, time: f32, value: V) {
        if self.loop_ {
            if time < self.max_time && time >= 0.0 {
                self.keys.insert(Key(time), value);
            }
        } else {
            self.keys.insert(Key(time), value);
        }
    }

    /// `getTotalTime` (`linear_animation.hpp:146-161`): 0 when empty (or
    /// flat), the key span when not looping, `maxTime` when looping.
    pub fn get_total_time(&self) -> f32 {
        let first = self.keys.keys().next().map(|k| k.0).unwrap_or(0.0);
        let last = self.keys.keys().next_back().map(|k| k.0).unwrap_or(0.0);
        if self.keys.is_empty() || almost_zero(last - first, 0.0004) {
            return 0.0;
        }

        if !self.loop_ {
            return last - first;
        }
        self.max_time
    }

    /// `loop(state, loopTime)` (`linear_animation.hpp:163-167`).
    pub fn loop_(&mut self, state: bool, loop_time: f32) {
        self.loop_ = state;
        self.max_time = loop_time;
    }

    /// `reset(loop, maxTime)` (`linear_animation.hpp:169-176`): clears the
    /// keys too.
    pub fn reset(&mut self, loop_: bool, max_time: f32) {
        self.loop_ = loop_;
        self.max_time = max_time;
        self.keys.clear();
    }
}

// ===========================================================================
// PulseLight
// ===========================================================================

/// `PulseLight` (`pulse_light.hpp:26-47`): an omni whose position rides a
/// Moo `Animation` (30 frames per second of animation time) and whose colour
/// is modulated by a `LinearAnimation<float>` of key frames.
pub struct PulseLight {
    light: OmniLightHandle,
    base_position: Vector3,
    base_colour: Colour,
    position_animation: Option<Rc<skeleton::Animation>>,
    colour_animation: LinearAnimation,
    position_anim_frame: f32,
    colour_anim_frame: f32,
}

pub type OmniLightHandle = Rc<RefCell<OmniLight>>;
pub type DirectionalLightHandle = Rc<RefCell<DirectionalLight>>;
pub type SpotLightHandle = Rc<RefCell<SpotLight>>;

impl PulseLight {
    /// `PulseLight(colour, position, innerRadius, outerRadius, animationName,
    /// animFrames, loopDuration)` (`pulse_light.cpp:17-46`): the anim frames
    /// are (time, value) pairs driving the colour; `loopDuration >= 0` makes
    /// the colour animation loop over the given duration (its own key span
    /// when 0).
    pub fn new(
        colour: Colour,
        position: Vector3,
        inner_radius: f32,
        outer_radius: f32,
        position_animation: Option<Rc<skeleton::Animation>>,
        anim_frames: &[(f32, f32)],
        loop_duration: f32,
    ) -> Self {
        let mut colour_animation = LinearAnimation::default();
        colour_animation.reset(false, 0.0);
        for (time, value) in anim_frames {
            colour_animation.add_key(*time, *value);
        }
        if loop_duration >= 0.0 {
            let anim_duration = if loop_duration > 0.0 {
                loop_duration
            } else {
                colour_animation.get_total_time()
            };
            colour_animation.loop_(true, anim_duration);
        }

        PulseLight {
            light: Rc::new(RefCell::new(OmniLight::with(
                colour,
                position,
                inner_radius,
                outer_radius,
            ))),
            base_position: position,
            base_colour: colour,
            position_animation,
            colour_animation,
            position_anim_frame: 0.0,
            colour_anim_frame: 0.0,
        }
    }

    /// The omni handle to add to containers; `tick`'s writes land in it.
    pub fn light(&self) -> OmniLightHandle {
        Rc::clone(&self.light)
    }

    /// `tick` (`pulse_light.cpp:49-79`).
    pub fn tick(&mut self, d_time: f32) {
        if let Some(animation) = &self.position_animation {
            self.position_anim_frame += d_time * 30.0;
            let total_time = animation.total_time();
            if self.position_anim_frame >= total_time {
                self.position_anim_frame %= total_time;
            }

            let mut res = Matrix::identity();
            for channel in animation.channels() {
                // The reference pre-multiplies whatever `result` left in the
                // matrix; the port skips channels that yield no matrix.
                if let Some(m) = channel.result(self.position_anim_frame) {
                    res.pre_multiply(&m);
                }
            }

            let anim_position = res.apply_to_origin();
            let position = self.base_position + anim_position;
            let mut light = self.light.borrow_mut();
            light.position = position;
            light.world_position = position;
        }

        self.colour_anim_frame += d_time;
        self.colour_anim_frame %= self.colour_animation.get_total_time();
        let colour_mod = self.colour_animation.animate(self.colour_anim_frame);
        let mut light = self.light.borrow_mut();
        light.colour = Colour::new(
            self.base_colour.r * colour_mod,
            self.base_colour.g * colour_mod,
            self.base_colour.b * colour_mod,
            1.0,
        );
    }
}

// ===========================================================================
// LightContainer
// ===========================================================================

/// `LightContainer` (`light_container.hpp:31-90`): a bucket of lights grouped
/// to light regions of a world, with helpers to sort, filter and merge them.
pub struct LightContainer {
    ambient_colour: Colour,
    directional_lights: Vec<DirectionalLightHandle>,
    omni_lights: Vec<OmniLightHandle>,
    spot_lights: Vec<SpotLightHandle>,
}

/// `SortOmnis` (`light_container.cpp:38-56`): priority descending first,
/// attenuation against the box descending second.
fn sort_omnis(omnis: &mut [OmniLightHandle], bb: &BoundBox) {
    omnis.sort_by(|p1, p2| {
        let pri1 = p1.borrow().priority();
        let pri2 = p2.borrow().priority();
        if pri1 != pri2 {
            pri2.cmp(&pri1)
        } else {
            let a1 = p1.borrow().attenuation(bb);
            let a2 = p2.borrow().attenuation(bb);
            a2.partial_cmp(&a1).unwrap_or(std::cmp::Ordering::Equal)
        }
    });
}

/// `SortSpots` (`light_container.cpp:59-77`): the same ordering for spots.
fn sort_spots(spots: &mut [SpotLightHandle], bb: &BoundBox) {
    spots.sort_by(|p1, p2| {
        let pri1 = p1.borrow().priority();
        let pri2 = p2.borrow().priority();
        if pri1 != pri2 {
            pri2.cmp(&pri1)
        } else {
            let a1 = p1.borrow().attenuation(bb);
            let a2 = p2.borrow().attenuation(bb);
            a2.partial_cmp(&a1).unwrap_or(std::cmp::Ordering::Equal)
        }
    });
}

impl LightContainer {
    /// The default constructor (`light_container.cpp:25-29`): dim ambient
    /// (0.1, 0.1, 0.1, 0).
    pub fn new() -> Self {
        LightContainer {
            ambient_colour: Colour::new(0.1, 0.1, 0.1, 0.0),
            directional_lights: Vec::new(),
            omni_lights: Vec::new(),
            spot_lights: Vec::new(),
        }
    }

    /// `assign` (`light_container.cpp:80-86`): copy every bucket and the
    /// ambient colour.
    pub fn assign(&mut self, source: &LightContainer) {
        self.ambient_colour = source.ambient_colour;
        self.spot_lights = source.spot_lights.clone();
        self.omni_lights = source.omni_lights.clone();
        self.directional_lights = source.directional_lights.clone();
    }

    /// `clear` (`light_container.cpp:88-94`): black-with-alpha ambient and
    /// empty buckets.
    pub fn clear(&mut self) {
        self.ambient_colour = Colour::new(0.0, 0.0, 0.0, 1.0);
        self.spot_lights.clear();
        self.omni_lights.clear();
        self.directional_lights.clear();
    }

    /// `init` (`light_container.cpp:96-173`): copy the ambient and the
    /// directionals (popping past two when limited), keep only the omnis and
    /// spots intersecting `bb`, each sorted and capped at four / two when
    /// limited. A null source blacks everything out.
    pub fn init(
        &mut self,
        source: Option<&LightContainer>,
        bb: &BoundBox,
        limit_to_renderable: bool,
    ) {
        match source {
            Some(source) => {
                self.ambient_colour = source.ambient_colour;

                self.directional_lights = source.directional_lights.clone();
                while limit_to_renderable && self.directional_lights.len() > 2 {
                    self.directional_lights.pop();
                }

                self.omni_lights.clear();
                for light in &source.omni_lights {
                    if light.borrow().intersects(bb) {
                        self.omni_lights.push(Rc::clone(light));
                    }
                }

                sort_omnis(&mut self.omni_lights, bb);
                if limit_to_renderable && self.omni_lights.len() > 4 {
                    self.omni_lights.truncate(4);
                }

                self.spot_lights.clear();
                for light in &source.spot_lights {
                    if light.borrow_mut().intersects(bb) {
                        self.spot_lights.push(Rc::clone(light));
                    }
                }

                sort_spots(&mut self.spot_lights, bb);
                if limit_to_renderable && self.spot_lights.len() > 2 {
                    self.spot_lights.truncate(2);
                }
            }
            None => {
                self.ambient_colour = Colour::new(0.0, 0.0, 0.0, 0.0);
                self.directional_lights.clear();
                self.omni_lights.clear();
                self.spot_lights.clear();
            }
        }
    }

    pub fn ambient_colour(&self) -> &Colour {
        &self.ambient_colour
    }

    pub fn set_ambient_colour(&mut self, colour: Colour) {
        self.ambient_colour = colour;
    }

    pub fn directionals(&self) -> &[DirectionalLightHandle] {
        &self.directional_lights
    }

    pub fn omnis(&self) -> &[OmniLightHandle] {
        &self.omni_lights
    }

    pub fn spots(&self) -> &[SpotLightHandle] {
        &self.spot_lights
    }

    /// `addDirectional` (`light_container.ipp`): appends, or checks identity
    /// first when asked.
    pub fn add_directional(&mut self, light: DirectionalLightHandle, check_existing: bool) -> bool {
        if check_existing {
            if !self
                .directional_lights
                .iter()
                .any(|l| Rc::ptr_eq(l, &light))
            {
                self.directional_lights.push(light);
                return true;
            }
            return false;
        }
        self.directional_lights.push(light);
        true
    }

    /// `delDirectional` (`light_container.ipp`).
    pub fn del_directional(&mut self, light: &DirectionalLightHandle) -> bool {
        match self
            .directional_lights
            .iter()
            .position(|l| Rc::ptr_eq(l, light))
        {
            Some(i) => {
                self.directional_lights.remove(i);
                true
            }
            None => false,
        }
    }

    /// `addOmni` (`light_container.ipp`).
    pub fn add_omni(&mut self, light: OmniLightHandle, check_existing: bool) -> bool {
        if check_existing {
            if !self.omni_lights.iter().any(|l| Rc::ptr_eq(l, &light)) {
                self.omni_lights.push(light);
                return true;
            }
            return false;
        }
        self.omni_lights.push(light);
        true
    }

    /// `delOmni` (`light_container.ipp`).
    pub fn del_omni(&mut self, light: &OmniLightHandle) -> bool {
        match self.omni_lights.iter().position(|l| Rc::ptr_eq(l, light)) {
            Some(i) => {
                self.omni_lights.remove(i);
                true
            }
            None => false,
        }
    }

    /// `addSpot` (`light_container.ipp`).
    pub fn add_spot(&mut self, light: SpotLightHandle, check_existing: bool) -> bool {
        if check_existing {
            if !self.spot_lights.iter().any(|l| Rc::ptr_eq(l, &light)) {
                self.spot_lights.push(light);
                return true;
            }
            return false;
        }
        self.spot_lights.push(light);
        true
    }

    /// `delSpot` (`light_container.ipp`).
    pub fn del_spot(&mut self, light: &SpotLightHandle) -> bool {
        match self.spot_lights.iter().position(|l| Rc::ptr_eq(l, light)) {
            Some(i) => {
                self.spot_lights.remove(i);
                true
            }
            None => false,
        }
    }

    /// `addToSelf(pLC, addOmnis, addSpots, addDirectionals)`
    /// (`light_container.cpp:229-275`): append the chosen buckets, skipping
    /// lights already present by identity.
    pub fn add_to_self(
        &mut self,
        source: Option<&LightContainer>,
        add_omnis: bool,
        add_spots: bool,
        add_directionals: bool,
    ) {
        let source = match source {
            Some(source) => source,
            None => return,
        };

        if add_directionals {
            for light in &source.directional_lights {
                if !self.directional_lights.iter().any(|l| Rc::ptr_eq(l, light)) {
                    self.directional_lights.push(Rc::clone(light));
                }
            }
        }

        if add_omnis {
            for light in &source.omni_lights {
                if !self.omni_lights.iter().any(|l| Rc::ptr_eq(l, light)) {
                    self.omni_lights.push(Rc::clone(light));
                }
            }
        }

        if add_spots {
            for light in &source.spot_lights {
                if !self.spot_lights.iter().any(|l| Rc::ptr_eq(l, light)) {
                    self.spot_lights.push(Rc::clone(light));
                }
            }
        }
    }

    /// `addToSelf(pLC, bb, limitToRenderable)` (`light_container.cpp:282-352`):
    /// merge with intersection filtering, then re-sort and cap. The
    /// directional loop's guard runs *before* the push, so a limited merge
    /// can reach three directionals (the reference quirk).
    pub fn add_to_self_culled(
        &mut self,
        source: Option<&LightContainer>,
        bb: &BoundBox,
        limit_to_renderable: bool,
    ) {
        let source = match source {
            Some(source) => source,
            None => return,
        };

        let mut i = 0;
        while i < source.directional_lights.len()
            && !(limit_to_renderable && self.directional_lights.len() > 2)
        {
            let light = &source.directional_lights[i];
            if !self.directional_lights.iter().any(|l| Rc::ptr_eq(l, light)) {
                self.directional_lights.push(Rc::clone(light));
            }
            i += 1;
        }

        for light in &source.omni_lights {
            if light.borrow().intersects(bb)
                && !self.omni_lights.iter().any(|l| Rc::ptr_eq(l, light))
            {
                self.omni_lights.push(Rc::clone(light));
            }
        }

        sort_omnis(&mut self.omni_lights, bb);
        if limit_to_renderable && self.omni_lights.len() > 4 {
            self.omni_lights.truncate(4);
        }

        for light in &source.spot_lights {
            if light.borrow_mut().intersects(bb)
                && !self.spot_lights.iter().any(|l| Rc::ptr_eq(l, light))
            {
                self.spot_lights.push(Rc::clone(light));
            }
        }

        sort_spots(&mut self.spot_lights, bb);
        if limit_to_renderable && self.spot_lights.len() > 2 {
            self.spot_lights.truncate(2);
        }
    }
}

impl Default for LightContainer {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn v3(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    fn almost(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-5, "{a} != {b}");
    }

    fn almost_v(a: Vector3, b: Vector3) {
        assert!((a - b).length() < 1e-5, "{:?} != {:?}", a, b);
    }

    /// A synthetic `.animation` with one discrete channel: identity at frame
    /// 0, a (3,0,0) translation at frame 30.
    fn synthetic_animation() -> Rc<skeleton::Animation> {
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(&60.0f32.to_le_bytes()); // total time
        raw.extend_from_slice(&1i32.to_le_bytes());
        raw.extend_from_slice(b"a"); // identifier
        raw.extend_from_slice(&1i32.to_le_bytes());
        raw.extend_from_slice(b"b"); // internal identifier
        raw.extend_from_slice(&1i32.to_le_bytes()); // one binder
        raw.extend_from_slice(&0i32.to_le_bytes()); // type 0: discrete
        raw.extend_from_slice(&1i32.to_le_bytes());
        raw.extend_from_slice(b"c"); // channel identifier
        raw.extend_from_slice(&2u32.to_le_bytes()); // two keys
        for (time, translation) in [(0.0f32, [0.0f32; 3]), (30.0, [3.0, 0.0, 0.0])] {
            raw.extend_from_slice(&time.to_le_bytes());
            for i in 0..4 {
                for j in 0..4 {
                    let value = match (i, j) {
                        (3, 0) => translation[0],
                        (3, 1) => translation[1],
                        (3, 2) => translation[2],
                        (i, j) if i == j => 1.0,
                        _ => 0.0,
                    };
                    raw.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        Rc::new(skeleton::Animation::from_bytes(&raw).expect("synthetic animation loads"))
    }

    #[test]
    fn colour_is_four_float_channels() {
        let c = Colour::new(0.25, 0.5, 0.75, 1.0);
        assert_eq!(c.r, 0.25);
        assert_eq!(c.g, 0.5);
        assert_eq!(c.b, 0.75);
        assert_eq!(c.a, 1.0);
        assert_eq!(Colour::default(), Colour::new(0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn default_directional_points_down_z() {
        let d = DirectionalLight::new();
        assert_eq!(d.direction(), &v3(0.0, 0.0, 1.0));
        assert_eq!(d.world_direction(), &v3(0.0, 0.0, 1.0));
        assert_eq!(d.colour(), &Colour::default());
    }

    #[test]
    fn directional_world_transform_uses_the_inverse_transpose() {
        // A 45-degree direction under scale(2,1,1): the inverse-transpose
        // route normalises to (0.5, 1, 0), while the naive transform would
        // give (2, 1, 0) — only the inverse-transpose keeps the angle.
        let mut transform = Matrix::zero();
        transform.m[0] = [2.0, 0.0, 0.0, 0.0];
        transform.m[1] = [0.0, 1.0, 0.0, 0.0];
        transform.m[2] = [0.0, 0.0, 1.0, 0.0];
        transform.m[3] = [0.0, 0.0, 0.0, 1.0];

        let mut d = DirectionalLight::with(Colour::new(1.0, 1.0, 1.0, 1.0), v3(1.0, 1.0, 0.0));
        d.world_transform(&transform);
        let inv_sqrt5 = 1.0 / 5.0f32.sqrt();
        almost_v(*d.world_direction(), v3(inv_sqrt5, 2.0 * inv_sqrt5, 0.0));

        // The identity leaves the direction untouched.
        let mut d = DirectionalLight::default();
        d.world_transform(&Matrix::identity());
        almost_v(*d.world_direction(), v3(0.0, 0.0, 1.0));

        // A singular transform falls back to the raw matrix: invert fails,
        // the direction goes through the transform unchanged.
        let mut singular = Matrix::zero();
        singular.m[0] = [1.0, 0.0, 0.0, 0.0];
        singular.m[2] = [0.0, 0.0, 1.0, 0.0];
        singular.m[3] = [0.0, 0.0, 0.0, 1.0];
        let mut d = DirectionalLight::with(Colour::default(), v3(1.0, 0.0, 0.0));
        d.world_transform(&singular);
        almost_v(*d.world_direction(), v3(1.0, 0.0, 0.0));

        // Direction/colour setters round-trip.
        d.set_direction(v3(1.0, 0.0, 0.0));
        d.set_colour(Colour::new(0.5, 0.5, 0.5, 1.0));
        assert_eq!(d.direction(), &v3(1.0, 0.0, 0.0));
        assert_eq!(d.colour(), &Colour::new(0.5, 0.5, 0.5, 1.0));
    }

    #[test]
    fn default_omni_is_a_dark_point_at_the_origin() {
        let o = OmniLight::new();
        assert_eq!(o.position(), &v3(0.0, 0.0, 0.0));
        assert_eq!(o.inner_radius(), 0.0);
        assert_eq!(o.outer_radius(), 0.0);
        assert_eq!(o.colour(), &Colour::default());
        assert_eq!(o.priority(), 0);
        assert_eq!(o.world_position(), &v3(0.0, 0.0, 0.0));
        assert_eq!(o.world_inner_radius(), 0.0);
        assert_eq!(o.world_outer_radius(), 0.0);
    }

    #[test]
    fn omni_world_transform_moves_scales_and_grows_the_bounds() {
        let mut o = OmniLight::with(
            Colour::new(1.0, 0.5, 0.25, 1.0),
            v3(1.0, 2.0, 3.0),
            1.0,
            4.0,
        );

        // Scale by two about the origin then translate by (10, 0, 0) — built
        // row-wise because `set_translate` resets the matrix.
        let mut transform = Matrix::zero();
        transform.m[0] = [2.0, 0.0, 0.0, 0.0];
        transform.m[1] = [0.0, 2.0, 0.0, 0.0];
        transform.m[2] = [0.0, 0.0, 2.0, 0.0];
        transform.m[3] = [10.0, 0.0, 0.0, 1.0];
        o.world_transform(&transform);

        almost_v(*o.world_position(), v3(12.0, 4.0, 6.0));
        almost(o.world_inner_radius(), 2.0);
        almost(o.world_outer_radius(), 8.0);

        // The world bounds are the local sphere box through the transform:
        // (−3,−2,−1)-(5,6,7) scaled and shifted.
        almost_v(o.world_bounds().min, v3(4.0, -4.0, -2.0));
        almost_v(o.world_bounds().max, v3(20.0, 12.0, 14.0));

        // The world state starts mirroring the locals.
        let fresh = OmniLight::with(Colour::default(), v3(1.0, 2.0, 3.0), 1.0, 4.0);
        almost_v(*fresh.world_position(), v3(1.0, 2.0, 3.0));
        almost(fresh.world_inner_radius(), 1.0);
        almost(fresh.world_outer_radius(), 4.0);
    }

    #[test]
    fn omni_inner_outer_radii_clamp_each_other() {
        let mut o = OmniLight::with(Colour::default(), v3(0.0, 0.0, 0.0), 1.0, 4.0);
        o.set_inner_radius(5.0);
        assert_eq!(o.outer_radius(), 5.0, "inner drags outer up");
        o.set_outer_radius(3.0);
        assert_eq!(o.inner_radius(), 3.0, "outer drags inner down");
    }

    #[test]
    fn omni_attenuation_ramps_over_the_radii() {
        let mut o = OmniLight::with(Colour::default(), v3(0.0, 0.0, 0.0), 2.0, 6.0);
        o.world_transform(&Matrix::identity());

        // Inside the inner radius: 2 - d/inner.
        let near = BoundBox::new(v3(1.0, -1.0, -1.0), v3(1.0, 1.0, 1.0));
        almost(o.attenuation(&near), 1.5);

        // Between the radii: linear from 1 down to 0.
        let mid = BoundBox::new(v3(5.0, -1.0, -1.0), v3(5.0, 1.0, 1.0));
        almost(o.attenuation(&mid), 0.25);

        // Beyond the outer radius: dark.
        let far = BoundBox::new(v3(100.0, -1.0, -1.0), v3(101.0, 1.0, 1.0));
        almost(o.attenuation(&far), 0.0);
    }

    #[test]
    fn omni_intersects_probes_the_outer_sphere() {
        let mut o = OmniLight::with(Colour::default(), v3(0.0, 0.0, 0.0), 1.0, 5.0);
        o.world_transform(&Matrix::identity());

        let inside = BoundBox::new(v3(1.0, 1.0, 1.0), v3(2.0, 2.0, 2.0));
        assert!(o.intersects(&inside));
        let outside = BoundBox::new(v3(10.0, 10.0, 10.0), v3(11.0, 11.0, 11.0));
        assert!(!o.intersects(&outside));
    }

    #[test]
    fn omni_priority_setter_round_trips() {
        let mut o = OmniLight::default();
        o.set_priority(7);
        assert_eq!(o.priority(), 7);
    }

    #[test]
    fn default_spot_points_down_z_with_a_closed_cone() {
        let s = SpotLight::new();
        assert_eq!(s.direction(), &v3(0.0, 0.0, 1.0));
        assert_eq!(s.cos_cone_angle(), 1.0);
        assert_eq!(s.world_direction(), &v3(0.0, 0.0, 1.0));
        assert!(s.dirty, "bounds start dirty");

        let s = SpotLight::with(
            Colour::new(1.0, 1.0, 1.0, 1.0),
            v3(0.0, 0.0, 0.0),
            v3(0.0, 0.0, 1.0),
            1.0,
            10.0,
            (std::f32::consts::FRAC_PI_4).cos(),
        );
        assert_eq!(s.world_inner_radius(), 1.0);
        assert_eq!(s.world_outer_radius(), 10.0);
        assert!(s.dirty);
    }

    #[test]
    fn spot_world_transform_rotates_the_direction_and_flags_dirty() {
        let mut s = SpotLight::with(
            Colour::default(),
            v3(1.0, 0.0, 0.0),
            v3(0.0, 0.0, 1.0),
            1.0,
            10.0,
            1.0,
        );
        // Rotate about x by -90 degrees (+z maps to +y), translate by
        // (0, 5, 0) — built row-wise because `set_translate` resets.
        let mut transform = Matrix::zero();
        transform.m[0] = [1.0, 0.0, 0.0, 0.0];
        transform.m[1] = [0.0, 0.0, -1.0, 0.0];
        transform.m[2] = [0.0, 1.0, 0.0, 0.0];
        transform.m[3] = [0.0, 5.0, 0.0, 1.0];
        s.world_transform(&transform);

        assert!(s.dirty);
        almost_v(*s.world_position(), v3(1.0, 5.0, 0.0));
        almost_v(*s.world_direction(), v3(0.0, 1.0, 0.0));

        // Clearing the flag via a bounds update.
        s.update_internal_bounds();
        assert!(!s.dirty);
    }

    #[test]
    fn spot_setters_flag_the_bounds_dirty() {
        let mut s = SpotLight::new();
        s.update_internal_bounds();
        assert!(!s.dirty);

        s.set_direction(v3(1.0, 0.0, 0.0));
        assert!(s.dirty);
        s.update_internal_bounds();
        s.set_outer_radius(20.0);
        assert!(s.dirty);
        s.update_internal_bounds();
        s.set_cos_cone_angle(0.5);
        assert!(s.dirty);

        s.update_internal_bounds();
        s.set_inner_radius(30.0);
        assert_eq!(s.outer_radius(), 30.0, "the mutual clamp also applies");
    }

    #[test]
    fn spot_update_internal_bounds_builds_the_cone_box_and_view() {
        let mut s = SpotLight::with(
            Colour::default(),
            v3(0.0, 0.0, 0.0),
            v3(0.0, 0.0, 1.0),
            1.0,
            10.0,
            (std::f32::consts::FRAC_PI_4).cos(),
        );
        s.world_transform(&Matrix::identity());

        // Angle pi/4: the cap sits at z=10 with radius 10*tan(pi/4) = 10.
        // The z-normal plane's basis puts xdir on +x and ydir on +y, so the
        // bounds span [-10, 10] on x/y and [0, 10] on z.
        let bounds = s.world_bounds();
        almost_v(bounds.min, v3(-10.0, -10.0, 0.0));
        almost_v(bounds.max, v3(10.0, 10.0, 10.0));

        // The light view is lookAt x perspective(2*angle, 1, 0, 10): the
        // origin maps to the frustum origin.
        let origin = s.light_view.apply_point(&v3(0.0, 0.0, 0.0));
        almost_v(origin, v3(0.0, 0.0, 0.0));
    }

    #[test]
    fn spot_intersects_gates_on_sphere_cone_box_and_frustum() {
        let mut s = SpotLight::with(
            Colour::default(),
            v3(0.0, 0.0, 0.0),
            v3(0.0, 0.0, 1.0),
            1.0,
            10.0,
            (std::f32::consts::FRAC_PI_4).cos(),
        );
        s.world_transform(&Matrix::identity());

        // A box inside the cone passes all three gates.
        let inside = BoundBox::new(v3(-1.0, -1.0, 3.0), v3(0.0, 0.0, 4.0));
        assert!(s.intersects(&inside));

        // Outside the outer sphere: first gate.
        let beyond_sphere = BoundBox::new(v3(100.0, 100.0, 0.5), v3(101.0, 101.0, 1.0));
        assert!(!s.intersects(&beyond_sphere));

        // Inside the sphere and the cone bounds but outside the actual
        // frustum (|x| > z): third gate.
        let frustum_miss = BoundBox::new(v3(-6.0, -6.0, 3.0), v3(-5.0, -5.0, 4.0));
        assert!(!s.intersects(&frustum_miss));

        // Inside the sphere but past the cone box on z: second gate.
        let behind = BoundBox::new(v3(-1.0, -1.0, -5.0), v3(0.0, 0.0, -4.0));
        assert!(!s.intersects(&behind));
    }

    #[test]
    fn spot_attenuation_matches_the_omni_ramp() {
        let mut s = SpotLight::with(
            Colour::default(),
            v3(0.0, 0.0, 0.0),
            v3(0.0, 0.0, 1.0),
            2.0,
            6.0,
            1.0,
        );
        s.world_transform(&Matrix::identity());

        let near = BoundBox::new(v3(1.0, -1.0, -1.0), v3(1.0, 1.0, 1.0));
        almost(s.attenuation(&near), 1.5);
        let far = BoundBox::new(v3(100.0, -1.0, -1.0), v3(101.0, 1.0, 1.0));
        almost(s.attenuation(&far), 0.0);
    }

    #[test]
    fn linear_animation_interpolates_and_loops() {
        let mut anim = LinearAnimation::new(false, 0.0);
        anim.add_key(0.0, 0.5);
        anim.add_key(30.0, 1.0);

        assert_eq!(anim.get_total_time(), 30.0);
        assert_eq!(anim.animate(0.0), 0.5);
        assert_eq!(anim.animate(30.0), 1.0);
        assert_eq!(anim.animate(15.0), 0.75);
        assert_eq!(anim.animate(-1.0), 0.5, "before the first key: first value");
        assert_eq!(anim.animate(31.0), 1.0, "past the last key: last value");

        // Looping over the key span.
        anim.loop_(true, 30.0);
        assert_eq!(anim.get_total_time(), 30.0);
        assert_eq!(anim.animate(0.0), 0.5);
        // Time 29 blends key 0 (0.5) towards key 30 (1.0): no wrap
        // adjustments fire (firstTime 0 <= time), fraction 29/30.
        assert!((anim.animate(29.0) - (0.5 + 0.5 * (29.0 / 30.0))).abs() < 1e-6);

        // While looping, keys outside [0, maxTime) are rejected.
        let mut looped = LinearAnimation::new(true, 10.0);
        looped.add_key(5.0, 1.0);
        looped.add_key(15.0, 2.0);
        assert_eq!(looped.keys.len(), 1);
        assert_eq!(looped.animate(0.0), 1.0);

        // A flat animation reports zero total time.
        let mut flat = LinearAnimation::default();
        flat.add_key(4.0, 7.0);
        assert_eq!(flat.get_total_time(), 0.0);
        assert_eq!(flat.animate(100.0), 7.0, "a single key always reads out");
        assert_eq!(flat.animate(0.0), 7.0);

        // reset clears everything.
        flat.reset(false, 0.0);
        assert_eq!(flat.animate(0.0), 0.0);
    }

    #[test]
    fn linear_animation_loop_seam_uses_the_wrapped_segment() {
        // A loop whose last key is before maxTime blends through the seam.
        let mut anim = LinearAnimation::new(true, 10.0);
        anim.add_key(0.0, 1.0);
        anim.add_key(5.0, 3.0);

        // Time 7.5 sits past the last key: the wrap takes key 0 as the upper
        // bound, bumped to time 10, so the fraction is (7.5 - 5) / (10 - 5)
        // = 0.5 — time itself is NOT bumped (firstTime <= time).
        assert!((anim.animate(7.5) - (3.0 + (1.0 - 3.0) * 0.5)).abs() < 1e-6);

        // Negative time wraps forward too: -2.5 -> 7.5.
        assert!((anim.animate(-2.5) - anim.animate(7.5)).abs() < 1e-6);

        // Hitting a key exactly reads the key.
        assert_eq!(anim.animate(5.0), 3.0);
    }

    #[test]
    fn pulse_light_modulates_colour_by_the_key_frames() {
        let base = Colour::new(1.0, 0.5, 0.25, 0.0);
        let mut pulse = PulseLight::new(
            base,
            v3(0.0, 0.0, 0.0),
            1.0,
            10.0,
            None,
            &[(0.0, 0.5), (30.0, 1.0)],
            30.0,
        );

        pulse.tick(15.0);
        let colour = *pulse.light().borrow().colour();
        almost(colour.r, 0.75);
        almost(colour.g, 0.375);
        almost(colour.b, 0.1875);
        assert_eq!(colour.a, 1.0, "the alpha is forced to one");

        // Without a position animation the light stays at its base position.
        assert_eq!(pulse.light().borrow().position(), &v3(0.0, 0.0, 0.0));
    }

    #[test]
    fn pulse_light_rides_the_position_animation_at_thirty_fps() {
        let base = Colour::new(1.0, 1.0, 1.0, 1.0);
        let mut pulse = PulseLight::new(
            base,
            v3(1.0, 2.0, 3.0),
            1.0,
            10.0,
            Some(synthetic_animation()),
            &[(0.0, 1.0)],
            10.0,
        );

        // One second of wall time is thirty frames: the channel's frame-30
        // key is a (3, 0, 0) translation.
        pulse.tick(1.0);
        {
            let handle = pulse.light();
            let light = handle.borrow();
            almost_v(*light.position(), v3(4.0, 2.0, 3.0));
            almost_v(*light.world_position(), v3(4.0, 2.0, 3.0));
        }

        // Both the local position and the cached world position update.
        let handle = pulse.light();
        assert_eq!(
            *handle.borrow().position(),
            *handle.borrow().world_position()
        );
    }

    #[test]
    fn pulse_light_position_animation_wraps_at_the_total_time() {
        // An animation of total time 30: one second of ticks reaches frame
        // 30, which wraps to 0 and the identity key applies.
        let mut raw: Vec<u8> = Vec::new();
        raw.extend_from_slice(&30.0f32.to_le_bytes());
        raw.extend_from_slice(&1i32.to_le_bytes());
        raw.extend_from_slice(b"a");
        raw.extend_from_slice(&1i32.to_le_bytes());
        raw.extend_from_slice(b"b");
        raw.extend_from_slice(&1i32.to_le_bytes());
        raw.extend_from_slice(&0i32.to_le_bytes());
        raw.extend_from_slice(&1i32.to_le_bytes());
        raw.extend_from_slice(b"c");
        raw.extend_from_slice(&2u32.to_le_bytes());
        for (time, translation) in [(0.0f32, [0.0f32; 3]), (29.0, [5.0, 0.0, 0.0])] {
            raw.extend_from_slice(&time.to_le_bytes());
            for i in 0..4 {
                for j in 0..4 {
                    let value = match (i, j) {
                        (3, 0) => translation[0],
                        (3, 1) => translation[1],
                        (3, 2) => translation[2],
                        (i, j) if i == j => 1.0,
                        _ => 0.0,
                    };
                    raw.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        let animation = Rc::new(skeleton::Animation::from_bytes(&raw).expect("loads"));

        let mut pulse = PulseLight::new(
            Colour::new(1.0, 1.0, 1.0, 1.0),
            v3(0.0, 0.0, 0.0),
            1.0,
            10.0,
            Some(animation),
            &[(0.0, 1.0)],
            10.0,
        );
        pulse.tick(1.0);
        assert_eq!(pulse.light().borrow().position(), &v3(0.0, 0.0, 0.0));
    }

    #[test]
    fn pulse_light_colour_animation_degenerates() {
        // A loop duration of 0 loops over the animation's own key span.
        let mut pulse = PulseLight::new(
            Colour::new(1.0, 1.0, 1.0, 1.0),
            v3(0.0, 0.0, 0.0),
            1.0,
            10.0,
            None,
            &[(0.0, 0.5), (30.0, 1.0)],
            0.0,
        );
        pulse.tick(15.0);
        almost(pulse.light().borrow().colour().r, 0.75);

        // A negative loop duration never loops.
        let mut pulse = PulseLight::new(
            Colour::new(1.0, 1.0, 1.0, 1.0),
            v3(0.0, 0.0, 0.0),
            1.0,
            10.0,
            None,
            &[(0.0, 0.5), (30.0, 1.0)],
            -1.0,
        );
        pulse.tick(15.0);
        almost(pulse.light().borrow().colour().r, 0.75);

        // A single key reads out regardless of the accumulated time.
        let mut pulse = PulseLight::new(
            Colour::new(1.0, 1.0, 1.0, 1.0),
            v3(0.0, 0.0, 0.0),
            1.0,
            10.0,
            None,
            &[(0.0, 0.25)],
            -1.0,
        );
        pulse.tick(5.0);
        almost(pulse.light().borrow().colour().r, 0.25);

        // No keys at all: the light dims to black.
        let mut pulse = PulseLight::new(
            Colour::new(1.0, 1.0, 1.0, 1.0),
            v3(0.0, 0.0, 0.0),
            1.0,
            10.0,
            None,
            &[],
            -1.0,
        );
        pulse.tick(5.0);
        let colour = *pulse.light().borrow().colour();
        almost(colour.r, 0.0);
        almost(colour.g, 0.0);
        almost(colour.b, 0.0);
        assert_eq!(colour.a, 1.0);
    }

    #[test]
    fn default_container_has_dim_ambient() {
        let c = LightContainer::new();
        assert_eq!(c.ambient_colour(), &Colour::new(0.1, 0.1, 0.1, 0.0));
        assert_eq!(c.directionals().len(), 0);
        assert_eq!(c.omnis().len(), 0);
        assert_eq!(c.spots().len(), 0);
        assert_eq!(
            LightContainer::default().ambient_colour(),
            c.ambient_colour()
        );
    }

    #[test]
    fn assign_copies_every_bucket_and_the_ambient() {
        let mut source = LightContainer::new();
        source.set_ambient_colour(Colour::new(0.2, 0.3, 0.4, 0.5));
        source.add_directional(Rc::new(RefCell::new(DirectionalLight::new())), false);
        source.add_omni(
            Rc::new(RefCell::new(OmniLight::with(
                Colour::default(),
                v3(0.0, 0.0, 0.0),
                1.0,
                5.0,
            ))),
            false,
        );
        source.add_spot(Rc::new(RefCell::new(SpotLight::new())), false);

        let mut target = LightContainer::new();
        target.assign(&source);
        assert_eq!(target.ambient_colour(), source.ambient_colour());
        assert_eq!(target.directionals().len(), 1);
        assert_eq!(target.omnis().len(), 1);
        assert_eq!(target.spots().len(), 1);
        // The handles are shared, not copied.
        assert!(Rc::ptr_eq(&target.omnis()[0], &source.omnis()[0]));
    }

    #[test]
    fn clear_blacks_out_the_ambient_and_empties_the_buckets() {
        let mut source = LightContainer::new();
        source.add_omni(
            Rc::new(RefCell::new(OmniLight::with(
                Colour::default(),
                v3(0.0, 0.0, 0.0),
                1.0,
                5.0,
            ))),
            false,
        );
        let mut target = LightContainer::new();
        target.assign(&source);

        target.clear();
        assert_eq!(target.ambient_colour(), &Colour::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(target.omnis().len(), 0);
    }

    #[test]
    fn init_from_none_blacks_everything_out() {
        let mut c = LightContainer::new();
        c.init(None, &BoundBox::default(), false);
        assert_eq!(c.ambient_colour(), &Colour::new(0.0, 0.0, 0.0, 0.0));
        assert_eq!(c.omnis().len(), 0);
        assert_eq!(c.directionals().len(), 0);
        assert_eq!(c.spots().len(), 0);
    }

    /// An omni at `position` with outer radius 5, world-transformed at the
    /// identity, carrying `priority` and `colour_scale`.
    fn omni_at(position: Vector3, priority: i32) -> OmniLightHandle {
        let mut o = OmniLight::with(Colour::default(), position, 1.0, 5.0);
        o.set_priority(priority);
        o.world_transform(&Matrix::identity());
        Rc::new(RefCell::new(o))
    }

    #[test]
    fn init_filters_by_intersection_sorts_by_priority_and_attenuation() {
        let mut source = LightContainer::new();
        // Priority 1 close, priority 2 far, priority 0 very close, one light
        // outside the region box.
        let close_low = omni_at(v3(0.0, 0.0, 0.0), 0);
        let close_high = omni_at(v3(1.0, 0.0, 0.0), 1);
        let far_high = omni_at(v3(4.5, 0.0, 0.0), 2);
        let outside = omni_at(v3(100.0, 0.0, 0.0), 3);
        source.add_omni(close_low.clone(), false);
        source.add_omni(close_high.clone(), false);
        source.add_omni(far_high.clone(), false);
        source.add_omni(outside, false);

        // Directionals: three, to exercise the pop-to-two cap.
        source.add_directional(Rc::new(RefCell::new(DirectionalLight::new())), false);
        source.add_directional(Rc::new(RefCell::new(DirectionalLight::new())), false);
        source.add_directional(Rc::new(RefCell::new(DirectionalLight::new())), false);

        let region = BoundBox::new(v3(-5.0, -5.0, -5.0), v3(5.0, 5.0, 5.0));
        let mut target = LightContainer::new();
        target.init(Some(&source), &region, true);

        // All three intersecting lights survive (distance 0, 1 and 4.5 all
        // probe inside the outer radius 5); sorted by priority descending,
        // ties by attenuation descending: far_high(4.5 away, att 1/8),
        // close_high(1 away, att 1.8), close_low(0 away, att 2).
        assert_eq!(target.omnis().len(), 3);
        assert!(Rc::ptr_eq(&target.omnis()[0], &far_high));
        assert!(Rc::ptr_eq(&target.omnis()[1], &close_high));
        assert!(Rc::ptr_eq(&target.omnis()[2], &close_low));

        // The outside light is filtered.
        assert!(!target.omnis().iter().any(|l| {
            let o = l.borrow();
            o.world_position().x > 50.0
        }));

        // Directionals pop to two.
        assert_eq!(target.directionals().len(), 2);

        // Without the limit everything survives.
        let mut target = LightContainer::new();
        target.init(Some(&source), &region, false);
        assert_eq!(target.omnis().len(), 3);
        assert_eq!(target.directionals().len(), 3);
    }

    #[test]
    fn init_sorts_equal_priority_by_attenuation() {
        let mut source = LightContainer::new();
        // Both priority 1; the near light sits in the region (distance 0,
        // attenuation 2) and the far light 3 units out (attenuation 0.5).
        let near = omni_at(v3(0.0, 0.0, 0.0), 1);
        let far = omni_at(v3(8.0, 0.0, 0.0), 1);
        source.add_omni(far.clone(), false);
        source.add_omni(near.clone(), false);

        let region = BoundBox::new(v3(-5.0, -5.0, -5.0), v3(5.0, 5.0, 5.0));
        let mut target = LightContainer::new();
        target.init(Some(&source), &region, false);

        // Same priority: the brighter (nearer) light sorts first.
        assert!(Rc::ptr_eq(&target.omnis()[0], &near));
        assert!(Rc::ptr_eq(&target.omnis()[1], &far));
    }

    #[test]
    fn init_caps_spots_at_two_and_filters_by_the_cone() {
        let mut source = LightContainer::new();
        for _ in 0..3 {
            source.add_spot(
                Rc::new(RefCell::new(SpotLight::with(
                    Colour::default(),
                    v3(0.0, 0.0, 0.0),
                    v3(0.0, 0.0, 1.0),
                    1.0,
                    20.0,
                    (std::f32::consts::FRAC_PI_4).cos(),
                ))),
                false,
            );
        }
        // A spot far outside the region.
        let mut far = SpotLight::with(
            Colour::default(),
            v3(100.0, 0.0, 0.0),
            v3(0.0, 0.0, 1.0),
            1.0,
            10.0,
            1.0,
        );
        far.world_transform(&Matrix::identity());
        source.add_spot(Rc::new(RefCell::new(far)), false);

        let region = BoundBox::new(v3(-5.0, -5.0, -5.0), v3(5.0, 5.0, 5.0));
        let mut target = LightContainer::new();
        target.init(Some(&source), &region, true);

        assert_eq!(target.spots().len(), 2, "the spot bucket caps at two");

        // Unlimitted keeps the three intersecting spots.
        let mut target = LightContainer::new();
        target.init(Some(&source), &region, false);
        assert_eq!(target.spots().len(), 3);
    }

    #[test]
    fn add_and_del_round_trip_by_identity() {
        let mut c = LightContainer::new();
        let d = Rc::new(RefCell::new(DirectionalLight::new()));
        let o = omni_at(v3(0.0, 0.0, 0.0), 0);
        let s = Rc::new(RefCell::new(SpotLight::new()));

        assert!(c.add_directional(d.clone(), false));
        assert!(c.add_omni(o.clone(), false));
        assert!(c.add_spot(s.clone(), false));

        // checkExisting rejects a second insert of the same handle.
        assert!(!c.add_directional(d.clone(), true));
        assert!(!c.add_omni(o.clone(), true));
        assert!(!c.add_spot(s.clone(), true));

        // A distinct handle passes the check.
        let other = omni_at(v3(1.0, 0.0, 0.0), 0);
        assert!(c.add_omni(other, true));

        assert!(c.del_directional(&d));
        assert!(c.del_omni(&o));
        assert!(c.del_spot(&s));

        // Deleting again misses.
        assert!(!c.del_directional(&d));
        assert!(!c.del_omni(&o));
        assert!(!c.del_spot(&s));
    }

    #[test]
    fn add_to_self_merges_with_identity_dedup_and_flags() {
        let mut source = LightContainer::new();
        let d = Rc::new(RefCell::new(DirectionalLight::new()));
        let o = omni_at(v3(0.0, 0.0, 0.0), 0);
        let s = Rc::new(RefCell::new(SpotLight::new()));
        source.add_directional(d.clone(), false);
        source.add_omni(o.clone(), false);
        source.add_spot(s.clone(), false);

        let mut target = LightContainer::new();
        target.add_to_self(Some(&source), true, true, true);
        assert_eq!(target.directionals().len(), 1);
        assert_eq!(target.omnis().len(), 1);
        assert_eq!(target.spots().len(), 1);

        // A second merge is a no-op: every light is already present.
        target.add_to_self(Some(&source), true, true, true);
        assert_eq!(target.directionals().len(), 1);
        assert_eq!(target.omnis().len(), 1);
        assert_eq!(target.spots().len(), 1);

        // The flags select which buckets merge.
        let mut partial = LightContainer::new();
        partial.add_to_self(Some(&source), true, false, false);
        assert_eq!(partial.omnis().len(), 1);
        assert_eq!(partial.directionals().len(), 0);
        assert_eq!(partial.spots().len(), 0);

        // A null source leaves the target untouched.
        partial.add_to_self(None, true, true, true);
        assert_eq!(partial.omnis().len(), 1);
    }

    #[test]
    fn add_to_self_culled_merges_sorts_and_caps() {
        let mut source = LightContainer::new();
        // Directionals: five in the source; the guard-before-push quirk
        // reaches three in a limited merge.
        for _ in 0..5 {
            source.add_directional(Rc::new(RefCell::new(DirectionalLight::new())), false);
        }
        // Omnis: one close with low priority, one farther with high priority,
        // one outside the region.
        let close = omni_at(v3(0.0, 0.0, 0.0), 0);
        let far_high = omni_at(v3(4.5, 0.0, 0.0), 2);
        let outside = omni_at(v3(100.0, 0.0, 0.0), 5);
        source.add_omni(close.clone(), false);
        source.add_omni(far_high.clone(), false);
        source.add_omni(outside, false);
        // Spots: three in the cone, capped at two when limited.
        for _ in 0..3 {
            source.add_spot(
                Rc::new(RefCell::new(SpotLight::with(
                    Colour::default(),
                    v3(0.0, 0.0, 0.0),
                    v3(0.0, 0.0, 1.0),
                    1.0,
                    20.0,
                    (std::f32::consts::FRAC_PI_4).cos(),
                ))),
                false,
            );
        }

        let region = BoundBox::new(v3(-5.0, -5.0, -5.0), v3(5.0, 5.0, 5.0));
        let mut target = LightContainer::new();
        target.add_to_self_culled(Some(&source), &region, true);

        assert_eq!(
            target.directionals().len(),
            3,
            "the guard-before-push quirk"
        );
        assert_eq!(target.omnis().len(), 2, "the outside omni is filtered");
        assert!(
            Rc::ptr_eq(&target.omnis()[0], &far_high),
            "sorted by priority"
        );
        assert_eq!(target.spots().len(), 2);

        // Merging again re-sorts the combined bucket and keeps the cap.
        target.add_to_self_culled(Some(&source), &region, true);
        assert_eq!(target.omnis().len(), 2, "dedup keeps the bucket at the cap");

        // Unlimited merges keep everything that intersects.
        let mut unlimited = LightContainer::new();
        unlimited.add_to_self_culled(Some(&source), &region, false);
        assert_eq!(unlimited.directionals().len(), 5);
        assert_eq!(unlimited.omnis().len(), 2);
        assert_eq!(unlimited.spots().len(), 3);

        // A null source is a no-op.
        unlimited.add_to_self_culled(None, &region, true);
        assert_eq!(unlimited.omnis().len(), 2);
    }

    #[test]
    fn box_outside_clip_space_rejects_boxes_past_the_frustum() {
        // The identity matrix is the canonical clip cube [-1,1]³ (w = 1).
        let identity = Matrix::identity();

        let inside = BoundBox::new(v3(-0.5, -0.5, 0.1), v3(0.5, 0.5, 0.5));
        assert!(!box_outside_clip_space(&inside, &identity));

        // Wholly to the right of the x = w plane: every corner shares the
        // right rejection.
        let right = BoundBox::new(v3(2.0, -0.5, 0.1), v3(3.0, 0.5, 0.5));
        assert!(box_outside_clip_space(&right, &identity));

        // Straddling the plane: some corner is inside, so no shared
        // rejection.
        let straddling = BoundBox::new(v3(0.5, -0.5, 0.1), v3(1.5, 0.5, 0.5));
        assert!(!box_outside_clip_space(&straddling, &identity));

        // Behind the near plane (z < 0) everywhere.
        let near_miss = BoundBox::new(v3(-0.5, -0.5, -2.0), v3(0.5, 0.5, -1.0));
        assert!(box_outside_clip_space(&near_miss, &identity));
    }

    #[test]
    fn omni_and_spot_accessors_round_trip() {
        let mut o = OmniLight::new();
        o.set_position(v3(1.0, 2.0, 3.0));
        o.set_colour(Colour::new(0.1, 0.2, 0.3, 0.4));
        o.set_priority(9);
        o.set_inner_radius(2.0);
        o.set_outer_radius(8.0);
        assert_eq!(*o.position(), v3(1.0, 2.0, 3.0));
        assert_eq!(*o.colour(), Colour::new(0.1, 0.2, 0.3, 0.4));
        assert_eq!(o.priority(), 9);
        assert_eq!(o.inner_radius(), 2.0);
        assert_eq!(o.outer_radius(), 8.0);

        let mut s = SpotLight::default();
        s.set_position(v3(4.0, 5.0, 6.0));
        s.set_colour(Colour::new(1.0, 1.0, 1.0, 1.0));
        s.set_inner_radius(2.0);
        s.set_outer_radius(8.0);
        s.set_cos_cone_angle(0.25);
        s.set_priority(3);
        assert_eq!(*s.position(), v3(4.0, 5.0, 6.0));
        assert_eq!(*s.colour(), Colour::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(s.priority(), 3);
        assert_eq!(s.inner_radius(), 2.0);
        assert_eq!(s.outer_radius(), 8.0);
        assert_eq!(s.cos_cone_angle(), 0.25);

        // Raising the inner radius drags the outer one up in the spot too.
        s.set_inner_radius(12.0);
        assert_eq!(s.outer_radius(), 12.0);
        // Lowering the outer radius drags the inner one down.
        s.set_outer_radius(5.0);
        assert_eq!(s.inner_radius(), 5.0);
    }

    #[test]
    fn sorting_prefers_higher_priority_first() {
        let mut source = LightContainer::new();
        let low = omni_at(v3(0.0, 0.0, 0.0), 0);
        let high = omni_at(v3(1.0, 0.0, 0.0), 5);
        source.add_omni(low.clone(), false);
        source.add_omni(high.clone(), false);

        let region = BoundBox::new(v3(-5.0, -5.0, -5.0), v3(5.0, 5.0, 5.0));
        let mut target = LightContainer::new();
        target.init(Some(&source), &region, false);
        assert!(Rc::ptr_eq(&target.omnis()[0], &high));
        assert!(Rc::ptr_eq(&target.omnis()[1], &low));

        let mut target = LightContainer::new();
        target.add_to_self_culled(Some(&source), &region, false);
        assert!(Rc::ptr_eq(&target.omnis()[0], &high));

        // Spots sort the same way: priority first, attenuation to break ties.
        fn spot_at(priority: i32, x: f32) -> SpotLightHandle {
            let mut s = SpotLight::with(
                Colour::default(),
                v3(x, 0.0, 0.0),
                v3(0.0, 0.0, 1.0),
                1.0,
                20.0,
                (std::f32::consts::FRAC_PI_4).cos(),
            );
            s.set_priority(priority);
            s.world_transform(&Matrix::identity());
            Rc::new(RefCell::new(s))
        }

        let mut spots = LightContainer::new();
        let spot_low = spot_at(0, 0.0);
        let spot_high = spot_at(7, 1.0);
        spots.add_spot(spot_low.clone(), false);
        spots.add_spot(spot_high.clone(), false);

        let mut target = LightContainer::new();
        target.init(Some(&spots), &region, false);
        assert!(Rc::ptr_eq(&target.spots()[0], &spot_high));
        assert!(Rc::ptr_eq(&target.spots()[1], &spot_low));
    }

    #[test]
    fn adding_with_check_existing_appends_a_new_handle() {
        let mut c = LightContainer::new();
        let d = Rc::new(RefCell::new(DirectionalLight::new()));
        let o = omni_at(v3(0.0, 0.0, 0.0), 0);
        let s = Rc::new(RefCell::new(SpotLight::new()));

        assert!(c.add_directional(d.clone(), true));
        assert!(c.add_omni(o.clone(), true));
        assert!(c.add_spot(s.clone(), true));
        assert_eq!(c.directionals().len(), 1);
        assert_eq!(c.omnis().len(), 1);
        assert_eq!(c.spots().len(), 1);
    }

    #[test]
    fn spots_attenuate_over_their_band_and_cap_omnis_at_four() {
        let mut s = SpotLight::with(
            Colour::default(),
            v3(0.0, 0.0, 0.0),
            v3(0.0, 0.0, 1.0),
            2.0,
            6.0,
            (std::f32::consts::FRAC_PI_4).cos(),
        );
        s.world_transform(&Matrix::identity());

        // Distance 3 between the radii 2 and 6: (3-6)/(2-6) = 0.75.
        let mid = BoundBox::new(v3(3.0, -1.0, -1.0), v3(3.0, 1.0, 1.0));
        almost(s.attenuation(&mid), 0.75);

        // Five intersecting omnis: limited init and limited merges both keep
        // only the four best.
        let region = BoundBox::new(v3(-5.0, -5.0, -5.0), v3(5.0, 5.0, 5.0));
        let mut source = LightContainer::new();
        let top = omni_at(v3(0.0, 0.0, 0.0), 4);
        let others = [1.0f32, 2.0, 3.0, 4.0];
        for (i, x) in others.iter().enumerate() {
            source.add_omni(omni_at(v3(*x, 0.0, 0.0), i as i32), false);
        }
        source.add_omni(top.clone(), false);

        let mut target = LightContainer::new();
        target.init(Some(&source), &region, true);
        assert_eq!(target.omnis().len(), 4, "init caps the omnis at four");
        assert!(Rc::ptr_eq(&target.omnis()[0], &top));

        let mut target = LightContainer::new();
        target.add_to_self_culled(Some(&source), &region, true);
        assert_eq!(target.omnis().len(), 4, "the merge caps the omnis at four");
    }

    #[test]
    fn linear_animation_wraps_before_the_first_key() {
        // A loop whose first key sits at 3: querying before it takes the last
        // key as the lower bound and bumps the time by maxTime.
        let mut anim = LinearAnimation::new(true, 10.0);
        anim.add_key(3.0, 0.25);
        anim.add_key(5.0, 0.75);
        assert!((anim.animate(0.5) - (0.75 + (0.25 - 0.75) * (5.5 / 8.0))).abs() < 1e-6);
    }

    #[test]
    fn vector_animation_interpolates_and_zeroes_like_the_float_one() {
        // The day/night cycle instantiates the same template with Vector3:
        // an empty track returns Value(0), keys mix component-wise.
        let empty = LinearAnimation::<Vector3>::new(true, 24.0);
        assert!(empty.empty());
        assert_eq!(empty.animate(12.0), Vector3::ZERO);

        let mut track = LinearAnimation::<Vector3>::new(true, 24.0);
        track.add_key(6.0, Vector3::new(0.0, 0.0, 0.0));
        track.add_key(18.0, Vector3::new(255.0, 255.0, 255.0));
        assert!(!track.empty());
        assert_eq!(track.animate(6.0), Vector3::new(0.0, 0.0, 0.0));
        let noon = track.animate(12.0);
        assert!((noon.x - 127.5).abs() < 1e-4);
        assert!((noon.y - 127.5).abs() < 1e-4);
        assert!((noon.z - 127.5).abs() < 1e-4);

        track.clear();
        assert!(track.empty());
        assert_eq!(track.animate(0.0), Vector3::ZERO);
    }

    #[test]
    fn clip_space_keys_and_far_plane_rejections() {
        // The map key compares by total order, matching `std::map<float,..>`.
        assert_eq!(
            Key(1.0).partial_cmp(&Key(2.0)),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            Key(2.0).partial_cmp(&Key(1.0)),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            Key(1.0).partial_cmp(&Key(1.0)),
            Some(std::cmp::Ordering::Equal)
        );

        // Every corner past z > w shares the far rejection.
        let far = BoundBox::new(v3(-0.5, -0.5, 2.0), v3(0.5, 0.5, 3.0));
        assert!(box_outside_clip_space(&far, &Matrix::identity()));
    }
}
