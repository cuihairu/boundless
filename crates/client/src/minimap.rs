//! The minimap projection core (`client/minimap.cpp` + `minimap.hpp`).
//!
//! A `Minimap` is a `SimpleGUIComponent` that draws a scrolling map with
//! markers ("entries") on it. What this module ports is the projection
//! core — everything that turns world-space marker positions into
//! clip-space marker positions and map-texture clip regions:
//!
//! - [`Minimap::update`] — the per-frame tick: zoom interpolation, the
//!   aspect-corrected rotation matrix, the simple-entry and entry
//!   projections, the cell-boundary lines, the texture-layer clip regions
//!   and the mouse-entry picking. It returns the frame's observable
//!   outputs as a [`MinimapFrame`].
//! - [`Minimap::add`]/[`Minimap::add_simple`]/[`Minimap::remove`] — the
//!   marker registry (handles start at 2, `0` means "no entry").
//! - [`Minimap::add_texture_layer`] and friends — the world-anchored map
//!   texture layers and their clip regions.
//! - [`Minimap::do_mouse_pick`] — the entry under the cursor.
//!
//! NOT ported (documented gaps): the D3D drawing itself (materials, custom
//! meshes, dynamic vertex buffers, draw calls — `drawSelf`), the
//! `SimpleGUIComponent` base machinery (`reSort`, `runTimeTransform`
//! bookkeeping, the `layout` system) and the `DataSection` `load`/`save`
//! bindings (GUI data). This module models only the projection math the
//! acceptance cares about ("地图投影及标记位置正确").
//!
//! Reference quirks kept on purpose:
//!
//! - The camera-relative swap `pos.y = pos.z; pos.z = 0` turns world
//!   (x, z) into the map's (x, y) with y pointing DOWN the map.
//! - Entry visibility uses `<= currentRange_ * (1 - margin)` while simple
//!   entries use the strict `<` — the reference is not uniform here.
//! - The marker rotation reads the yaw of the marker's ORIGINAL matrix
//!   before the projected matrix is written back (`pMatrix->matrix(m)`).
//! - Handles wrap at 2^32 (`currHandle_` is a `uint32`).
//! - `isZero` (degenerate axes) is a strict `== 0`, not an epsilon test;
//!   `almostZero` (point-size scaling) is `|f| < 0.0004`.

use std::cell::RefCell;
use std::ops::{Add, Div, DivAssign, Mul, Sub};
use std::rc::Rc;

use crate::simple_gui::{Vector2, Vector3};

/// `Math::clamp(minValue, value, maxValue)` (mathdef.hpp:223) — note the
/// argument order: minimum FIRST.
fn clamp(min: f32, value: f32, max: f32) -> f32 {
    value.max(min).min(max)
}

/// `-epsilon < f < epsilon` (stdmf.hpp:616) — used by the point-size
/// scaling test.
fn almost_zero(f: f32) -> bool {
    f > -0.0004 && f < 0.0004
}

/// `f32::MAX` stands in for `FLT_MAX` (the "no entry" distance).
const FLT_MAX: f32 = f32::MAX;

/// `CELL_MAX_VALUE` (minimap.cpp:670) — cell-boundary floats are clamped
/// into ±this before the lines are built.
const CELL_MAX_VALUE: f32 = 100_000.0;

/// The cell-boundary line colour (`0x80808080`, minimap.cpp:701).
const CELL_VERTEX_COLOUR: u32 = 0x8080_8080;

// ---------------------------------------------------------------------------
// Vectors and matrices
// ---------------------------------------------------------------------------

/// A 4-component vector. As a colour it is `(r, g, b, a)`; as a clip
/// region it is `(left, top, right, bottom)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vector4 {
    /// First component (red / left).
    pub x: f32,
    /// Second component (green / top).
    pub y: f32,
    /// Third component (blue / right).
    pub z: f32,
    /// Fourth component (alpha / bottom).
    pub w: f32,
}

impl Vector4 {
    /// A new vector.
    pub fn new(x: f32, y: f32, z: f32, w: f32) -> Vector4 {
        Vector4 { x, y, z, w }
    }
}

/// `Colour::getUint32(const Vector4&)` (colour.hpp:116): each component
/// truncated to an int, clamped to 0..255, packed ARGB.
pub fn colour_to_u32(c: Vector4) -> u32 {
    let r = clamp(0.0, c.x, 255.0) as u32;
    let g = clamp(0.0, c.y, 255.0) as u32;
    let b = clamp(0.0, c.z, 255.0) as u32;
    let a = clamp(0.0, c.w, 255.0) as u32;
    (a << 24) | (r << 16) | (g << 8) | b
}

/// A 4×4 matrix, row-major (`m[row][col]`, `m00` == `m[0][0]`). Row 3 is
/// the translation (`applyToOrigin`), column 2 the local z axis
/// (`applyToUnitAxisVector(2)`), the layout the reference's Matrix uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    /// The matrix elements, row-major.
    pub m: [[f32; 4]; 4],
}

impl Matrix {
    /// The identity matrix.
    pub fn identity() -> Matrix {
        Matrix {
            m: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    /// `Matrix::setScale` (matrix.cpp:285).
    pub fn set_scale(&mut self, x: f32, y: f32, z: f32) {
        self.m = [
            [x, 0.0, 0.0, 0.0],
            [0.0, y, 0.0, 0.0],
            [0.0, 0.0, z, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
    }

    /// `Matrix::setTranslate` (matrix.cpp:317).
    pub fn set_translate(&mut self, x: f32, y: f32, z: f32) {
        self.m = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [x, y, z, 1.0],
        ];
    }

    /// `Matrix::setRotateZ` (matrix.cpp:484).
    pub fn set_rotate_z(&mut self, angle: f32) {
        let sa = angle.sin();
        let ca = angle.cos();
        self.m = [
            [ca, sa, 0.0, 0.0],
            [-sa, ca, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
    }

    /// `Matrix::setRow` (matrix.hpp) — row-major row replacement.
    pub fn set_row(&mut self, row: usize, value: Vector4) {
        self.m[row] = [value.x, value.y, value.z, value.w];
    }

    /// `Matrix::preMultiply` (matrix.cpp:381): `self = other * self`.
    pub fn pre_multiply(&mut self, other: &Matrix) {
        *self = multiply(other, self);
    }

    /// `Matrix::postMultiply` (matrix.cpp:398): `self = self * other`.
    pub fn post_multiply(&mut self, other: &Matrix) {
        *self = multiply(self, other);
    }

    /// `Matrix::applyPoint` (matrix.ipp:159). BigWorld stores matrices
    /// row-major but reads them column-wise (the D3D convention): each
    /// output component gathers its COLUMN and adds row 3. A stored
    /// product `M1*M2` therefore applies `M1` first.
    pub fn apply_point(&self, v: Vector3) -> Vector3 {
        Vector3 {
            x: self.m[0][0] * v.x + self.m[1][0] * v.y + self.m[2][0] * v.z + self.m[3][0],
            y: self.m[0][1] * v.x + self.m[1][1] * v.y + self.m[2][1] * v.z + self.m[3][1],
            z: self.m[0][2] * v.x + self.m[1][2] * v.y + self.m[2][2] * v.z + self.m[3][2],
        }
    }

    /// `Matrix::applyToOrigin` (matrix.ipp:379): row 3, the translation.
    pub fn apply_to_origin(&self) -> Vector3 {
        Vector3 {
            x: self.m[3][0],
            y: self.m[3][1],
            z: self.m[3][2],
        }
    }

    /// `Matrix::applyToUnitAxisVector(axis)` (matrix.ipp:368): column
    /// `axis` as a vector.
    pub fn apply_to_unit_axis_vector(&self, axis: usize) -> Vector3 {
        Vector3 {
            x: self.m[0][axis],
            y: self.m[1][axis],
            z: self.m[2][axis],
        }
    }

    /// `Matrix::yaw` (matrix.ipp:638): `atan2` of the normalised local
    /// z axis.
    pub fn yaw(&self) -> f32 {
        let mut zdir = self.apply_to_unit_axis_vector(2);
        zdir.normalise();
        zdir.x.atan2(zdir.z)
    }

    /// A rotation about Z followed by a translation in one matrix.
    fn rotate_z_then_translate(angle: f32, x: f32, y: f32, z: f32) -> Matrix {
        let mut m = Matrix::identity();
        m.set_rotate_z(angle);
        m.pre_multiply(&{
            let mut t = Matrix::identity();
            t.set_translate(x, y, z);
            t
        });
        m
    }
}

impl Add<Vector3> for Vector3 {
    type Output = Vector3;
    fn add(self, rhs: Vector3) -> Vector3 {
        Vector3 {
            x: self.x + rhs.x,
            y: self.y + rhs.y,
            z: self.z + rhs.z,
        }
    }
}

impl Sub<Vector3> for Vector3 {
    type Output = Vector3;
    fn sub(self, rhs: Vector3) -> Vector3 {
        Vector3 {
            x: self.x - rhs.x,
            y: self.y - rhs.y,
            z: self.z - rhs.z,
        }
    }
}

impl Mul<f32> for Vector3 {
    type Output = Vector3;
    fn mul(self, rhs: f32) -> Vector3 {
        Vector3 {
            x: self.x * rhs,
            y: self.y * rhs,
            z: self.z * rhs,
        }
    }
}

impl DivAssign<f32> for Vector3 {
    fn div_assign(&mut self, rhs: f32) {
        self.x /= rhs;
        self.y /= rhs;
        self.z /= rhs;
    }
}

impl Div<f32> for Vector3 {
    type Output = Vector3;
    fn div(self, rhs: f32) -> Vector3 {
        Vector3 {
            x: self.x / rhs,
            y: self.y / rhs,
            z: self.z / rhs,
        }
    }
}

impl Vector3 {
    /// `Vector3::normalise` — divide by the length; a zero vector stays
    /// zero (the reference would produce NaN here; the port guards so the
    /// projection helpers stay total).
    fn normalise(&mut self) {
        let len = self.length();
        if len > 0.0 {
            *self /= len;
        }
    }

    /// `Vector3::length`.
    fn length(&self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }
}

/// `Matrix::multiply(m1, m2)` (matrix.cpp:348) — the plain row-major
/// product.
fn multiply(m1: &Matrix, m2: &Matrix) -> Matrix {
    let a = &m1.m;
    let b = &m2.m;
    let mut out = Matrix::identity();
    for i in 0..4 {
        for j in 0..4 {
            out.m[i][j] =
                a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j] + a[i][3] * b[3][j];
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Axis-aligned box line clipping
// ---------------------------------------------------------------------------

/// `AABB` (`BoundingBox`, boundbox.hpp:61): an axis-aligned box with a
/// `clip` that clips a line segment to the box interior.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    /// Minimum corner.
    pub min: Vector3,
    /// Maximum corner.
    pub max: Vector3,
}

impl Aabb {
    /// A new box from its extreme corners.
    pub fn new(min: Vector3, max: Vector3) -> Aabb {
        Aabb { min, max }
    }

    /// `AABB::clip` (boundbox.cpp:310): the Liang-Barsky style segment
    /// clip. Returns false when the segment misses the box (or runs
    /// exactly along a face outside it); otherwise rewrites `source` and
    /// `extent` to the clipped endpoints and returns true.
    ///
    /// The degenerate-axis handling is a strict `== 0` (`isZero`), not an
    /// epsilon test, and the `delta < 0` branch uses the same face order
    /// as the `delta > 0` branch — ported verbatim.
    pub fn clip(&self, source: &mut Vector3, extent: &mut Vector3, bloat: f32) -> bool {
        let mut s_min = 0.0f32;
        let mut e_max = 1.0f32;
        let delta = *extent - *source;

        // The x faces.
        if delta.x != 0.0 {
            let mt_x = (self.min.x - bloat - source.x) / delta.x;
            let mt_big_x = (self.max.x + bloat - source.x) / delta.x;
            if delta.x > 0.0 {
                if mt_x > s_min {
                    s_min = mt_x;
                }
                if mt_big_x < e_max {
                    e_max = mt_big_x;
                }
            } else {
                if mt_big_x > s_min {
                    s_min = mt_big_x;
                }
                if mt_x < e_max {
                    e_max = mt_x;
                }
            }
        } else if source.x < self.min.x - bloat || source.x >= self.max.x + bloat {
            return false;
        }

        // The y faces.
        if delta.y != 0.0 {
            let mt_y = (self.min.y - bloat - source.y) / delta.y;
            let mt_big_y = (self.max.y + bloat - source.y) / delta.y;
            if delta.y > 0.0 {
                if mt_y > s_min {
                    s_min = mt_y;
                }
                if mt_big_y < e_max {
                    e_max = mt_big_y;
                }
            } else {
                if mt_big_y > s_min {
                    s_min = mt_big_y;
                }
                if mt_y < e_max {
                    e_max = mt_y;
                }
            }
        } else if source.y < self.min.y - bloat || source.y >= self.max.y + bloat {
            return false;
        }

        // The z faces.
        if delta.z != 0.0 {
            let mt_z = (self.min.z - bloat - source.z) / delta.z;
            let mt_big_z = (self.max.z + bloat - source.z) / delta.z;
            if delta.z > 0.0 {
                if mt_z > s_min {
                    s_min = mt_z;
                }
                if mt_big_z < e_max {
                    e_max = mt_big_z;
                }
            } else {
                if mt_big_z > s_min {
                    s_min = mt_big_z;
                }
                if mt_z < e_max {
                    e_max = mt_z;
                }
            }
        } else if source.z < self.min.z - bloat || source.z >= self.max.z + bloat {
            return false;
        }

        if s_min >= e_max {
            return false;
        }

        *extent = *source + delta * e_max;
        *source = *source + delta * s_min;
        true
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// The component sizing modes (`SIZE_MODE_LEGACY` default,
/// simple_gui_component.cpp:1157-1162).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeMode {
    /// The value is already a clip fraction.
    Clip,
    /// The value is pixels.
    Pixel,
    /// The value is a raw coordinate (the default).
    Legacy,
}

/// The minimap's own size/position configuration. The reference reads
/// `SimpleGUI::instance()`'s screen resolution at use time; this port
/// takes it here so the projection stays pure and testable. The defaults
/// match a legacy-sized, legacy-positioned component (the reference
/// constructor's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinimapConfig {
    /// `width()`.
    pub width: f32,
    /// `widthMode()`.
    pub width_mode: SizeMode,
    /// `height()`.
    pub height: f32,
    /// `heightMode()`.
    pub height_mode: SizeMode,
    /// `position_` (legacy coordinates).
    pub position: Vector3,
    /// The x position mode.
    pub position_mode_x: PositionMode,
    /// The y position mode.
    pub position_mode_y: PositionMode,
    /// The screen resolution (for `pixelRangesToClip`).
    pub screen_resolution: Vector2,
}

impl Default for MinimapConfig {
    fn default() -> MinimapConfig {
        MinimapConfig {
            width: 1.0,
            height: 1.0,
            width_mode: SizeMode::Legacy,
            height_mode: SizeMode::Legacy,
            position: Vector3 {
                x: 0.5,
                y: -0.5,
                z: 0.0,
            },
            position_mode_x: PositionMode::Legacy,
            position_mode_y: PositionMode::Legacy,
            screen_resolution: Vector2 {
                x: 1024.0,
                y: 768.0,
            },
        }
    }
}

/// `SimpleGUI::pixelRangesToClip(1, 1, &w, &h)` (simple_gui.cpp:758): the
/// clip sizes of one pixel.
pub fn pixel_ranges_to_clip(w: f32, h: f32, screen: &Vector2) -> (f32, f32) {
    (w * 2.0 / screen.x, h * 2.0 / screen.y)
}

impl MinimapConfig {
    /// `widthInClip(relParentWidth)` (simple_gui_component.cpp:2672).
    pub fn width_in_clip(&self, rel_parent_width: f32) -> f32 {
        match self.width_mode {
            SizeMode::Clip => {
                pixel_ranges_to_clip(
                    (self.width / 2.0) * rel_parent_width,
                    0.0,
                    &self.screen_resolution,
                )
                .0
            }
            SizeMode::Pixel => pixel_ranges_to_clip(self.width, 0.0, &self.screen_resolution).0,
            SizeMode::Legacy => self.width,
        }
    }

    /// `heightInClip(relParentHeight)` (simple_gui_component.cpp:2725).
    pub fn height_in_clip(&self, rel_parent_height: f32) -> f32 {
        match self.height_mode {
            SizeMode::Clip => {
                pixel_ranges_to_clip(
                    0.0,
                    (self.height / 2.0) * rel_parent_height,
                    &self.screen_resolution,
                )
                .1
            }
            SizeMode::Pixel => pixel_ranges_to_clip(0.0, self.height, &self.screen_resolution).1,
            SizeMode::Legacy => self.height,
        }
    }

    /// `positionInClip(relParentWidth, relParentHeight)`
    /// (simple_gui_component.cpp:3107). `rel_parent_width` /
    /// `rel_parent_height` are the parent's pixel ranges; the CLIP mode
    /// maps the stored value through `parentSpaceToClipSpace` against the
    /// parent's clip size.
    pub fn position_in_clip(&self, rel_parent_width: f32, rel_parent_height: f32) -> Vector2 {
        let rp_width_clip =
            pixel_ranges_to_clip(rel_parent_width, rel_parent_height, &self.screen_resolution).0;
        let rp_height_clip =
            pixel_ranges_to_clip(rel_parent_width, rel_parent_height, &self.screen_resolution).1;
        let x = match self.position_mode_x {
            PositionMode::Legacy => self.position.x,
            PositionMode::Pixel => {
                let x_clip = pixel_ranges_to_clip(self.position.x, 0.0, &self.screen_resolution).0;
                x_clip - rp_width_clip / 2.0
            }
            PositionMode::Clip => parent_space_to_clip_space(self.position.x, rp_width_clip),
        };
        let y = match self.position_mode_y {
            PositionMode::Legacy => self.position.y,
            PositionMode::Pixel => {
                let y_clip = pixel_ranges_to_clip(0.0, self.position.y, &self.screen_resolution).1;
                -y_clip + rp_height_clip / 2.0
            }
            PositionMode::Clip => parent_space_to_clip_space(self.position.y, rp_height_clip),
        };
        Vector2 { x, y }
    }
}

/// The position modes (the reference's constructor default).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PositionMode {
    /// `POSITION_MODE_CLIP` — the value is a parent-relative clip
    /// coordinate mapped through `parentSpaceToClipSpace`.
    Clip,
    /// `POSITION_MODE_PIXEL`.
    Pixel,
    /// `POSITION_MODE_LEGACY` (default).
    Legacy,
}

/// `parentSpaceToClipSpace` (simple_gui_component.cpp:38).
fn parent_space_to_clip_space(v: f32, parent_size_in_clip: f32) -> f32 {
    let n = (v + 1.0) / 2.0;
    parent_size_in_clip * n - parent_size_in_clip / 2.0
}

/// The four corners of the minimap quad in clip coordinates, as
/// `clipBounds` yields them (simple_gui_component.cpp:4389) for the
/// LEGACY sizing this port models: top-left, top-right, bottom-right,
/// bottom-left.
type ClipCorners = (Vector2, Vector2, Vector2, Vector2);

// ---------------------------------------------------------------------------
// Entries and layers
// ---------------------------------------------------------------------------

/// A minimap entry with its own GUI component (`Minimap::add`). The
/// matrix supplies the world position and orientation; `visible` is the
/// in-range flag `updateEntries` toggles.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    /// The marker's world matrix.
    pub matrix: Matrix,
    /// Whether the marker is inside the current range.
    pub visible: bool,
}

/// A simple (batched, colour-only) entry (`Minimap::addSimple`).
#[derive(Clone, Debug, PartialEq)]
pub struct SimpleEntry {
    /// The marker's world matrix.
    pub matrix: Matrix,
    /// The packed colour.
    pub colour: u32,
}

/// A world-anchored map texture layer (`Minimap::addTextureLayer`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextureLayer {
    /// `worldSize_` — the world extent of the texture (metres; negative
    /// components flip the map).
    pub world_size: Vector2,
    /// `worldAnchor_` — the world centre of the texture.
    pub world_anchor: Vector2,
    /// The computed clip region `(left, top, right, bottom)`.
    pub clip_region: Vector4,
}

/// A simple entry's projected vertex (`updateSimpleEntries`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SimpleEntryVertex {
    /// The clip-space position (z = 0.5).
    pub position: Vector3,
    /// The texture coordinates `(0, 0)`.
    pub uv: Vector2,
    /// The packed colour.
    pub colour: u32,
}

/// A projected entry (`updateEntries`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntryProjection {
    /// The projected clip-space position.
    pub position: Vector3,
    /// The projected matrix (translation + orientation, aspect
    /// corrected).
    pub transform: Matrix,
    /// The in-range visibility flag.
    pub visible: bool,
}

/// A clipped cell-boundary vertex (`updateCellBoundaries`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellVertex {
    /// The clip-space position.
    pub position: Vector3,
    /// The texture coordinates.
    pub uv: Vector2,
    /// The line colour (`0x80808080`).
    pub colour: u32,
}

/// The cursor state passed into the pick path (the reference reads
/// `SimpleGUI::instance().mouseCursor()`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseCursorState {
    /// `MouseCursor::visible`.
    pub visible: bool,
    /// `MouseCursor::position`.
    pub position: Vector2,
}

/// One frame's projected outputs — the observable result of
/// [`Minimap::update`].
pub struct MinimapFrame {
    /// The camera position used this frame.
    pub cam_pos: Vector3,
    /// The camera yaw used for the rotation.
    pub cam_yaw: f32,
    /// The zoom-interpolated range after this frame's tick.
    pub current_range: f32,
    /// The `runTimeTransform` applied to the minimap itself (rotate
    /// mode); identity when the border child takes the rotation.
    pub transform: Matrix,
    /// The border child's transform in the non-rotate mode.
    pub border_transform: Option<Matrix>,
    /// The projected simple entries (handle, vertex), in handle order.
    pub simple_entries: Vec<(u32, SimpleEntryVertex)>,
    /// The projected entries (handle, projection), in handle order.
    pub entries: Vec<(u32, EntryProjection)>,
    /// The clipped cell-boundary vertices, in cell/edge order.
    pub cell_vertices: Vec<CellVertex>,
    /// The per-layer clip regions `(layer, region)` in draw order.
    pub clip_regions: Vec<(u32, Vector4)>,
}

/// The GUI script callbacks the pick path fires (`onEntryFocus` /
/// `onEntryBlur` with the entry handle).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MinimapCallbacks {
    log: Rc<RefCell<Vec<(String, u32)>>>,
}

impl MinimapCallbacks {
    /// A fresh recorder.
    pub fn new() -> MinimapCallbacks {
        MinimapCallbacks {
            log: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// The recorded `(callback, handle)` pairs in call order.
    pub fn calls(&self) -> Vec<(String, u32)> {
        self.log.borrow().clone()
    }

    fn push(&self, callback: &str, handle: u32) {
        self.log.borrow_mut().push((callback.to_string(), handle));
    }
}

// ---------------------------------------------------------------------------
// The minimap
// ---------------------------------------------------------------------------

/// The minimap (`client/minimap.cpp`).
///
/// Fields mirror the reference's private members one-for-one where the
/// projection reads them. `entries`/`simple_entries`/`texture_layers` are
/// ordered maps (`BW::map`, key-ordered; the multimap keeps equal keys
/// in insertion order).
pub struct Minimap {
    /// `range_`.
    pub range: f32,
    /// `currentRange_`.
    pub current_range: f32,
    /// `zoomSpeed_`.
    pub zoom_speed: f32,
    /// `rotate_`.
    pub rotate: bool,
    /// `currHandle_` — starts at 1, so the first handle is 2.
    curr_handle: u32,
    /// `mouseEntryPicking_`.
    pub mouse_entry_picking: bool,
    /// `pickedEntry_` — 0 means "none".
    picked_entry: u32,
    /// `simpleEntrySize_`.
    pub simple_entry_size: f32,
    /// `pointSizeScaling_`.
    pub point_size_scaling: Vector2,
    /// `simpleEntriesVisible_`.
    pub simple_entries_visible: bool,
    /// `cellBounds_` — the cell boundary floats, groups of 4.
    pub cell_bounds: Vec<f32>,
    /// `cellBoundsVisible_`.
    pub cell_bounds_visible: bool,
    /// `margin_`.
    pub margin: f32,
    /// The sizing/positioning configuration.
    pub config: MinimapConfig,
    /// The entries (`entries_`).
    entries: std::collections::BTreeMap<u32, Entry>,
    /// The simple entries (`simpleEntries_`).
    simple_entries: std::collections::BTreeMap<u32, SimpleEntry>,
    /// The texture layers in insertion order (`textureLayers_`, a
    /// multimap).
    texture_layers: Vec<(u32, TextureLayer)>,
    /// The script object (`pScriptObject_`) for the pick callbacks.
    script: Option<MinimapCallbacks>,
    /// The transform used by the NEXT frame's pick path (the reference
    /// saves `runTimeTransform()` before the base update resets it).
    run_time_transform: Matrix,
}

impl Default for Minimap {
    fn default() -> Minimap {
        Minimap::new(MinimapConfig::default())
    }
}

impl Minimap {
    /// The reference constructor (minimap.cpp:208).
    pub fn new(config: MinimapConfig) -> Minimap {
        Minimap {
            range: 500.0,
            current_range: 500.0,
            zoom_speed: 0.25,
            rotate: true,
            curr_handle: 1,
            mouse_entry_picking: false,
            picked_entry: 0,
            simple_entry_size: 10.0,
            point_size_scaling: Vector2 { x: 0.0, y: 0.0 },
            simple_entries_visible: true,
            cell_bounds: Vec::new(),
            cell_bounds_visible: true,
            margin: 0.05,
            config,
            entries: std::collections::BTreeMap::new(),
            simple_entries: std::collections::BTreeMap::new(),
            texture_layers: Vec::new(),
            script: None,
            run_time_transform: Matrix::identity(),
        }
    }

    /// `nextHandle` (minimap.cpp:1162): `currHandle_++`, wrapping at
    /// 2^32 (a `uint32`).
    fn next_handle(&mut self) -> u32 {
        self.curr_handle = self.curr_handle.wrapping_add(1);
        self.curr_handle
    }

    /// Test hook for the 2^32 wrap (the reference's `currHandle_` is a
    /// `uint32`).
    #[cfg(test)]
    pub(crate) fn set_curr_handle(&mut self, handle: u32) {
        self.curr_handle = handle;
    }

    /// The GUI script object for the pick callbacks.
    pub fn set_script(&mut self, script: MinimapCallbacks) {
        self.script = Some(script);
    }

    /// `pickedEntry_` — 0 means "none".
    pub fn picked_entry(&self) -> u32 {
        self.picked_entry
    }

    /// The registered entries, keyed by handle.
    pub fn entries(&self) -> &std::collections::BTreeMap<u32, Entry> {
        &self.entries
    }

    /// The registered simple entries, keyed by handle.
    pub fn simple_entries(&self) -> &std::collections::BTreeMap<u32, SimpleEntry> {
        &self.simple_entries
    }

    /// `add` (minimap.cpp:1111): an entry with its own GUI component.
    /// (The component itself is not modelled; the handle and matrix are
    /// what the projection consumes.)
    pub fn add(&mut self, matrix: Matrix) -> u32 {
        let handle = self.next_handle();
        self.entries.insert(
            handle,
            Entry {
                matrix,
                visible: true,
            },
        );
        handle
    }

    /// `addSimple` (minimap.cpp:1095): a batched colour-only entry.
    pub fn add_simple(&mut self, matrix: Matrix, colour: Vector4) -> u32 {
        let handle = self.next_handle();
        self.simple_entries.insert(
            handle,
            SimpleEntry {
                matrix,
                colour: colour_to_u32(colour),
            },
        );
        handle
    }

    /// `remove` (minimap.cpp:1135): an entry handle removes from
    /// `entries_` first, then `simpleEntries_`.
    pub fn remove(&mut self, handle: u32) {
        if self.entries.remove(&handle).is_some() {
            return;
        }
        self.simple_entries.remove(&handle);
    }

    /// `addTextureLayer` (minimap.cpp:1258).
    pub fn add_texture_layer(&mut self, layer: u32, texture: TextureLayer) {
        self.texture_layers.push((layer, texture));
    }

    /// `delTextureLayer` (minimap.cpp:1281): every layer with that
    /// number (a multimap erase).
    pub fn del_texture_layer(&mut self, layer: u32) {
        self.texture_layers.retain(|(key, _)| *key != layer);
    }

    /// `delAllTextureLayers` (minimap.cpp:1271).
    pub fn del_all_texture_layers(&mut self) {
        self.texture_layers.clear();
    }

    /// The point size the simple entries would be drawn with
    /// (`drawSelf`'s POINTSIZE logic, minimap.cpp:988-996).
    pub fn point_size(&self) -> f32 {
        if almost_zero(self.point_size_scaling.x) && almost_zero(self.point_size_scaling.y) {
            self.simple_entry_size
        } else {
            self.point_size_scaling.x + self.point_size_scaling.y * self.current_range
        }
    }

    /// `update` (minimap.cpp:285): one frame of projection.
    ///
    /// `cam` is the viewpoint matrix (the reference's `viewpoint_`, or
    /// `Moo::rc().invView()` when absent); `cursor` is the GUI cursor
    /// the pick path reads. Returns this frame's projected outputs.
    pub fn update(
        &mut self,
        cam: &Matrix,
        d_time: f32,
        rel_parent_width: f32,
        rel_parent_height: f32,
        cursor: &MouseCursorState,
    ) -> MinimapFrame {
        let cam_pos = cam.apply_to_origin();
        let mut fwd = cam.apply_to_unit_axis_vector(2);
        fwd.normalise();
        let cam_yaw = fwd.x.atan2(fwd.z);

        // The aspect correction (the GUI is drawn in clip space but the
        // screen is not square).
        let (w, h) = pixel_ranges_to_clip(1.0, 1.0, &self.config.screen_resolution);
        let mut pre_scale = Matrix::identity();
        pre_scale.set_scale(h / w, 1.0, 1.0);
        let mut post_scale = Matrix::identity();
        post_scale.set_scale(w / h, 1.0, 1.0);

        // The zoom interpolation.
        let amount = clamp(0.01, d_time / self.zoom_speed, 1.0);
        self.current_range += (self.range - self.current_range) * amount;

        // The transform the pick path will use next frame is the one
        // from the previous tick (the base update resets the
        // runTimeTransform).
        let old_transform = self.run_time_transform;

        let cell_vertices = if self.cell_bounds_visible {
            self.update_cell_boundaries(&cam_pos, rel_parent_width, rel_parent_height)
        } else {
            Vec::new()
        };
        let simple_entries = if self.simple_entries_visible {
            self.update_simple_entries(&cam_pos, rel_parent_width, rel_parent_height)
        } else {
            Vec::new()
        };
        let entries = self.update_entries(
            &cam_pos,
            &pre_scale,
            &post_scale,
            rel_parent_width,
            rel_parent_height,
        );

        let (transform, border_transform) = if self.rotate {
            let mut rel_rot = Matrix::identity();
            rel_rot.set_rotate_z(cam_yaw);
            rel_rot.pre_multiply(&pre_scale);
            rel_rot.post_multiply(&post_scale);
            self.run_time_transform = rel_rot;
            (rel_rot, None)
        } else {
            let mut rel_rot = Matrix::identity();
            rel_rot.set_rotate_z(cam_yaw);
            rel_rot.pre_multiply(&pre_scale);
            rel_rot.post_multiply(&post_scale);
            // The border child takes the rotation; the minimap itself
            // keeps the base update's positioning (not modelled here).
            self.run_time_transform = Matrix::identity();
            (Matrix::identity(), Some(rel_rot))
        };

        let clip_regions =
            self.calculate_clip_regions(rel_parent_width, rel_parent_height, &cam_pos);

        if self.mouse_entry_picking {
            let (tl, tr, br, bl) = self.clip_bounds(rel_parent_width, rel_parent_height);
            let z = self.config.position.z;
            let p = [
                old_transform.apply_point(Vector3 {
                    x: tl.x,
                    y: tl.y,
                    z,
                }),
                old_transform.apply_point(Vector3 {
                    x: tr.x,
                    y: tr.y,
                    z,
                }),
                old_transform.apply_point(Vector3 {
                    x: br.x,
                    y: br.y,
                    z,
                }),
                old_transform.apply_point(Vector3 {
                    x: bl.x,
                    y: bl.y,
                    z,
                }),
            ];
            let center = (p[0] + p[1] + p[2] + p[3]) / 4.0;
            let size = Vector2 {
                x: p[1].x - p[0].x,
                y: p[0].y - p[3].y,
            };
            self.do_mouse_pick(&cam_pos, &center, &size, cursor);
        }

        MinimapFrame {
            cam_pos,
            cam_yaw,
            current_range: self.current_range,
            transform,
            border_transform,
            simple_entries,
            entries,
            cell_vertices,
            clip_regions,
        }
    }

    /// The minimap quad's clip corners (`clipBounds`).
    fn clip_bounds(&self, rel_parent_width: f32, rel_parent_height: f32) -> ClipCorners {
        let tl = self
            .config
            .position_in_clip(rel_parent_width, rel_parent_height);
        let w = self.config.width_in_clip(rel_parent_width);
        let h = self.config.height_in_clip(rel_parent_height);
        (
            tl,
            Vector2 {
                x: tl.x + w,
                y: tl.y,
            },
            Vector2 {
                x: tl.x + w,
                y: tl.y + h,
            },
            Vector2 {
                x: tl.x,
                y: tl.y + h,
            },
        )
    }

    /// `updateSimpleEntries` (minimap.cpp:536).
    fn update_simple_entries(
        &self,
        cam_pos: &Vector3,
        rel_parent_width: f32,
        rel_parent_height: f32,
    ) -> Vec<(u32, SimpleEntryVertex)> {
        let mod_x = self.config.width_in_clip(rel_parent_width) / 2.0;
        let mod_y = self.config.height_in_clip(rel_parent_height) / 2.0;
        let mut out = Vec::new();
        for (handle, entry) in &self.simple_entries {
            let mut pos = entry.matrix.apply_to_origin() - *cam_pos;
            pos.y = pos.z;
            pos.z = 0.0;
            pos /= self.current_range;
            let dist = pos.length();
            // The strict `<` margin test (unlike updateEntries).
            if dist < 1.0 - self.margin {
                let mut vertex = SimpleEntryVertex {
                    position: pos,
                    uv: Vector2 { x: 0.0, y: 0.0 },
                    colour: entry.colour,
                };
                vertex.position.x *= mod_x;
                vertex.position.y *= mod_y;
                vertex.position.z = 0.5;
                out.push((*handle, vertex));
            }
        }
        out
    }

    /// `updateEntries` (minimap.cpp:590).
    fn update_entries(
        &mut self,
        cam_pos: &Vector3,
        pre_scale: &Matrix,
        post_scale: &Matrix,
        rel_parent_width: f32,
        rel_parent_height: f32,
    ) -> Vec<(u32, EntryProjection)> {
        let mod_x = self.config.width_in_clip(rel_parent_width) / 2.0;
        let mod_y = self.config.height_in_clip(rel_parent_height) / 2.0;
        let mut out = Vec::new();
        for (handle, entry) in self.entries.iter_mut() {
            // The yaw is read from the ORIGINAL matrix before the
            // projected matrix is written back.
            let yaw = entry.matrix.yaw();
            let mut rel_pos = entry.matrix.apply_to_origin() - *cam_pos;
            rel_pos.y = rel_pos.z;
            rel_pos.z = 0.0;
            let world_length = rel_pos.length();
            if world_length <= self.current_range * (1.0 - self.margin) {
                entry.visible = true;
                rel_pos /= self.current_range;
                rel_pos.z = 0.0;
                rel_pos.x *= mod_x;
                rel_pos.y *= mod_y;
                let mut m = Matrix::rotate_z_then_translate(0.0, rel_pos.x, rel_pos.y, rel_pos.z);
                let mut rel_rot = Matrix::identity();
                rel_rot.set_rotate_z(-yaw);
                rel_rot.pre_multiply(pre_scale);
                rel_rot.post_multiply(post_scale);
                m.pre_multiply(&rel_rot);
                entry.matrix = m;
            } else {
                entry.visible = false;
            }
            let position = entry.matrix.apply_to_origin();
            let transform = entry.matrix;
            let visible = entry.visible;
            out.push((
                *handle,
                EntryProjection {
                    position,
                    transform,
                    visible,
                },
            ));
        }
        out
    }

    /// `updateCellBoundaries` (minimap.cpp:658).
    fn update_cell_boundaries(
        &self,
        cam_pos: &Vector3,
        rel_parent_width: f32,
        rel_parent_height: f32,
    ) -> Vec<CellVertex> {
        let mod_x = self.config.width_in_clip(rel_parent_width) / 2.0;
        let mod_y = self.config.height_in_clip(rel_parent_height) / 2.0;
        let bounds = Aabb::new(
            Vector3 {
                x: -self.current_range,
                y: -5.0,
                z: -self.current_range,
            },
            Vector3 {
                x: self.current_range,
                y: 5.0,
                z: self.current_range,
            },
        );

        // Groups of 4, each a (bl.x, bl.y, tr.x, tr.y) cell rectangle.
        let mut cell_boundaries: Vec<Vector4> = Vec::new();
        let mut i = 3;
        while i < self.cell_bounds.len() {
            let a = clamp(-CELL_MAX_VALUE, self.cell_bounds[i - 3], CELL_MAX_VALUE);
            let b = clamp(-CELL_MAX_VALUE, self.cell_bounds[i - 2], CELL_MAX_VALUE);
            let c = clamp(-CELL_MAX_VALUE, self.cell_bounds[i - 1], CELL_MAX_VALUE);
            let d = clamp(-CELL_MAX_VALUE, self.cell_bounds[i], CELL_MAX_VALUE);
            cell_boundaries.push(Vector4::new(a, b, c, d));
            i += 4;
        }

        let mut out = Vec::new();
        for cell in &cell_boundaries {
            let bl = Vector2 {
                x: cell.x,
                y: cell.y,
            };
            let tr = Vector2 {
                x: cell.z,
                y: cell.w,
            };
            for j in 0..4 {
                let (mut line_a, mut line_b) = match j {
                    0 => (
                        Vector3 {
                            x: bl.x - cam_pos.x,
                            y: 0.0,
                            z: bl.y - cam_pos.z,
                        },
                        Vector3 {
                            x: tr.x - cam_pos.x,
                            y: 0.0,
                            z: bl.y - cam_pos.z,
                        },
                    ),
                    1 => (
                        Vector3 {
                            x: bl.x - cam_pos.x,
                            y: 0.0,
                            z: bl.y - cam_pos.z,
                        },
                        Vector3 {
                            x: bl.x - cam_pos.x,
                            y: 0.0,
                            z: tr.y - cam_pos.z,
                        },
                    ),
                    2 => (
                        Vector3 {
                            x: bl.x - cam_pos.x,
                            y: 0.0,
                            z: tr.y - cam_pos.z,
                        },
                        Vector3 {
                            x: tr.x - cam_pos.x,
                            y: 0.0,
                            z: tr.y - cam_pos.z,
                        },
                    ),
                    _ => (
                        Vector3 {
                            x: tr.x - cam_pos.x,
                            y: 0.0,
                            z: bl.y - cam_pos.z,
                        },
                        Vector3 {
                            x: tr.x - cam_pos.x,
                            y: 0.0,
                            z: tr.y - cam_pos.z,
                        },
                    ),
                };
                if bounds.clip(&mut line_a, &mut line_b, 0.0) {
                    for point in [line_a, line_b] {
                        // The map plane reads (x, z): world z becomes the
                        // map's y and the flat 0 becomes its z.
                        let mut q = Vector3 {
                            x: point.x,
                            y: point.z,
                            z: 0.0,
                        };
                        q /= self.current_range;
                        let uv = Vector2 {
                            x: q.x / 2.0 + 0.5,
                            y: q.y / 2.0 + 0.5,
                        };
                        q.x *= mod_x;
                        q.y *= mod_y;
                        out.push(CellVertex {
                            position: q,
                            uv,
                            colour: CELL_VERTEX_COLOUR,
                        });
                    }
                }
            }
        }
        out
    }

    /// `calculateClipRegions` (minimap.cpp:1041).
    fn calculate_clip_regions(
        &self,
        rel_parent_width: f32,
        rel_parent_height: f32,
        cam_pos: &Vector3,
    ) -> Vec<(u32, Vector4)> {
        let half_width = self.config.width_in_clip(rel_parent_width) / 2.0;
        let half_height = self.config.height_in_clip(rel_parent_height) / 2.0;
        let pos = self
            .config
            .position_in_clip(rel_parent_width, rel_parent_height);

        let mut out = Vec::new();
        for (layer, layer_data) in self.sorted_layers() {
            let anchor_x = layer_data.world_anchor.x - cam_pos.x;
            let anchor_y = layer_data.world_anchor.y - cam_pos.z;
            let mut res = Vector4 {
                x: anchor_x - layer_data.world_size.x / 2.0,
                y: anchor_y + layer_data.world_size.y / 2.0,
                z: 0.0,
                w: 0.0,
            };
            res.z = res.x + layer_data.world_size.x;
            res.w = res.y - layer_data.world_size.y;
            res.x = res.x / self.current_range * half_width + pos.x;
            res.y = res.y / self.current_range * half_height + pos.y;
            res.z = res.z / self.current_range * half_width + pos.x;
            res.w = res.w / self.current_range * half_height + pos.y;
            // The texture may be flipped in y, but the clip region must
            // not be.
            if layer_data.world_size.y < 0.0 {
                std::mem::swap(&mut res.w, &mut res.y);
            }
            out.push((layer, res));
        }
        out
    }

    /// The layers in draw order (the multimap's key order, equal keys
    /// in insertion order).
    fn sorted_layers(&self) -> Vec<(u32, TextureLayer)> {
        let mut indices: Vec<usize> = (0..self.texture_layers.len()).collect();
        indices.sort_by_key(|&i| self.texture_layers[i].0);
        indices
            .into_iter()
            .map(|i| self.texture_layers[i])
            .collect()
    }

    /// `doMousePick` (minimap.cpp:383).
    pub fn do_mouse_pick(
        &mut self,
        cam_pos: &Vector3,
        center: &Vector3,
        size: &Vector2,
        cursor: &MouseCursorState,
    ) {
        if !cursor.visible {
            self.on_entry_blur();
            return;
        }

        let mut mpos = Vector2 {
            x: cursor.position.x - center.x,
            y: cursor.position.y - center.y,
        };
        mpos.x /= size.x / 2.0;
        mpos.y /= size.y / 2.0;

        if mpos.x.abs() >= 1.0 - self.margin || mpos.y.abs() >= 1.0 - self.margin {
            self.on_entry_blur();
            return;
        }

        let mut best = FLT_MAX;
        let mut best_entry = 0;
        for (handle, entry) in &self.simple_entries {
            let dist = self.distance_to_entry(&mpos, cam_pos, &entry.matrix);
            if dist < best {
                best = dist;
                best_entry = *handle;
            }
        }

        let mut entry_size = self.simple_entry_size / self.config.screen_resolution.x;
        entry_size /= size.x / 2.0;

        if best > entry_size {
            best_entry = 0;
        }

        if best_entry != self.picked_entry {
            self.on_entry_blur();
            self.picked_entry = best_entry;
            self.on_entry_focus();
        }
    }

    /// `distanceToEntry` (minimap.cpp:456).
    fn distance_to_entry(&self, mpos: &Vector2, cpos: &Vector3, matrix: &Matrix) -> f32 {
        let mut pos = matrix.apply_to_origin() - *cpos;
        pos.y = pos.z;
        pos.z = 0.0;
        pos /= self.current_range;
        let dist = pos.length();
        if dist < 1.0 - self.margin {
            let mut d = pos;
            d.x -= mpos.x;
            d.y -= mpos.y;
            return d.length();
        }
        FLT_MAX
    }

    /// `onEntryBlur` (minimap.cpp:485).
    fn on_entry_blur(&mut self) {
        if self.picked_entry != 0 {
            if let Some(script) = &self.script {
                script.push("onEntryBlur", self.picked_entry);
            }
            self.picked_entry = 0;
        }
    }

    /// `onEntryFocus` (minimap.cpp:509).
    fn on_entry_focus(&mut self) {
        if self.picked_entry != 0
            && let Some(script) = &self.script
        {
            script.push("onEntryFocus", self.picked_entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translation(x: f32, y: f32, z: f32) -> Matrix {
        let mut m = Matrix::identity();
        m.set_translate(x, y, z);
        m
    }

    /// A world matrix at (x, z) with a yaw about the world Y axis: the
    /// local z axis points at (sin yaw, 0, cos yaw), which is what
    /// `Matrix::yaw` reads.
    fn yawed(yaw: f32, x: f32, z: f32) -> Matrix {
        let mut m = translation(x, 0.0, z);
        let (s, c) = yaw.sin_cos();
        m.m[0][2] = s;
        m.m[2][2] = c;
        m
    }

    fn cursor_at(x: f32, y: f32) -> MouseCursorState {
        MouseCursorState {
            visible: true,
            position: Vector2 { x, y },
        }
    }

    fn hidden_cursor() -> MouseCursorState {
        MouseCursorState {
            visible: false,
            position: Vector2::default(),
        }
    }

    fn v3(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3 { x, y, z }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn close3(a: Vector3, b: Vector3) -> bool {
        close(a.x, b.x) && close(a.y, b.y) && close(a.z, b.z)
    }

    fn close2(a: Vector2, b: Vector2) -> bool {
        close(a.x, b.x) && close(a.y, b.y)
    }

    #[test]
    fn matrix_multiplication_order_matches_the_reference() {
        let mut m = Matrix::identity();
        m.set_translate(10.0, 0.0, 0.0);
        let mut r = Matrix::identity();
        r.set_rotate_z(std::f32::consts::FRAC_PI_2);
        // applyPoint reads columns, so a stored product applies its LEFT
        // factor first. postMultiply appends on the right: translate
        // first, then the rotation (rotating the offset about the
        // world origin).
        let mut post = m;
        post.post_multiply(&r);
        assert!(close3(
            post.apply_point(v3(1.0, 0.0, 0.0)),
            v3(0.0, 11.0, 0.0)
        ));
        // preMultiply prepends on the left: the rotation happens first.
        let mut pre = m;
        pre.pre_multiply(&r);
        assert!(close3(
            pre.apply_point(v3(1.0, 0.0, 0.0)),
            v3(10.0, 1.0, 0.0)
        ));
    }

    #[test]
    fn apply_helpers_read_the_stored_layout() {
        let mut m = translation(3.0, 4.0, 5.0);
        assert!(close3(m.apply_to_origin(), v3(3.0, 4.0, 5.0)));
        assert!(close3(m.apply_point(v3(1.0, 2.0, 3.0)), v3(4.0, 6.0, 8.0)));
        // Column 2 of the identity is the local z axis.
        assert!(close3(
            Matrix::identity().apply_to_unit_axis_vector(2),
            v3(0.0, 0.0, 1.0)
        ));
        m.set_row(3, Vector4::new(7.0, 8.0, 9.0, 1.0));
        assert!(close3(m.apply_to_origin(), v3(7.0, 8.0, 9.0)));
        let mut s = Matrix::identity();
        s.set_scale(2.0, 3.0, 4.0);
        assert!(close3(s.apply_point(v3(1.0, 1.0, 1.0)), v3(2.0, 3.0, 4.0)));
    }

    #[test]
    fn yaw_reads_the_local_z_axis() {
        assert!(close(Matrix::identity().yaw(), 0.0));
        let m = yawed(std::f32::consts::FRAC_PI_4, 0.0, 0.0);
        assert!(close(m.yaw(), std::f32::consts::FRAC_PI_4));
    }

    #[test]
    fn pixel_ranges_to_clip_scales_by_resolution() {
        let screen = Vector2 {
            x: 1024.0,
            y: 768.0,
        };
        let (w, h) = pixel_ranges_to_clip(3.0, 5.0, &screen);
        assert!(close(w, 6.0 / 1024.0));
        assert!(close(h, 10.0 / 768.0));
    }

    #[test]
    fn size_and_position_modes_cover_all_three_kinds() {
        let screen = Vector2 {
            x: 1024.0,
            y: 768.0,
        };
        // Half-screen parent pixel ranges make rpClip exactly (1, 1).
        let rp_w = 512.0;
        let rp_h = 384.0;

        let mut cfg = MinimapConfig {
            screen_resolution: screen,
            ..MinimapConfig::default()
        };
        assert!(close(cfg.width_in_clip(rp_w), 1.0)); // LEGACY: raw.
        assert!(close(cfg.height_in_clip(rp_h), 1.0));

        cfg.width_mode = SizeMode::Clip;
        cfg.height_mode = SizeMode::Clip;
        assert!(close(cfg.width_in_clip(rp_w), 0.5)); // (w/2·rpW)·2/1024.
        assert!(close(cfg.height_in_clip(rp_h), 0.5));

        cfg.width_mode = SizeMode::Pixel;
        cfg.height_mode = SizeMode::Pixel;
        assert!(close(cfg.width_in_clip(rp_w), 2.0 / 1024.0));
        assert!(close(cfg.height_in_clip(rp_h), 2.0 / 768.0));

        cfg.position = v3(0.3, -0.7, 0.0);

        cfg.position_mode_x = PositionMode::Clip;
        cfg.position_mode_y = PositionMode::Clip;
        let p = cfg.position_in_clip(rp_w, rp_h);
        assert!(close(p.x, 0.15)); // ((0.3+1)/2)·1 - 0.5.
        assert!(close(p.y, -0.35)); // ((-0.7+1)/2)·1 - 0.5.

        cfg.position_mode_x = PositionMode::Pixel;
        cfg.position_mode_y = PositionMode::Pixel;
        let p = cfg.position_in_clip(rp_w, rp_h);
        assert!(close(p.x, 0.6 / 1024.0 - 0.5));
        assert!(close(p.y, 1.4 / 768.0 + 0.5));

        cfg.position_mode_x = PositionMode::Legacy;
        cfg.position_mode_y = PositionMode::Legacy;
        let p = cfg.position_in_clip(rp_w, rp_h);
        assert!(close2(p, Vector2 { x: 0.3, y: -0.7 }));
    }

    #[test]
    fn constructor_defaults_match_the_reference() {
        let m = Minimap::default();
        assert_eq!(m.range, 500.0);
        assert_eq!(m.current_range, 500.0);
        assert_eq!(m.zoom_speed, 0.25);
        assert!(m.rotate);
        assert!(!m.mouse_entry_picking);
        assert_eq!(m.picked_entry(), 0);
        assert_eq!(m.simple_entry_size, 10.0);
        assert_eq!(m.point_size_scaling, Vector2 { x: 0.0, y: 0.0 });
        assert!(m.simple_entries_visible);
        assert!(m.cell_bounds.is_empty());
        assert!(m.cell_bounds_visible);
        assert_eq!(m.margin, 0.05);
    }

    #[test]
    fn handles_start_at_two_and_wrap_at_u32() {
        let mut m = Minimap::default();
        assert_eq!(m.add(Matrix::identity()), 2);
        assert_eq!(
            m.add_simple(Matrix::identity(), Vector4::new(1.0, 1.0, 1.0, 1.0)),
            3
        );
        assert_eq!(m.add(Matrix::identity()), 4);
        // currHandle_ is a uint32 in the reference: it wraps.
        m.set_curr_handle(u32::MAX);
        assert_eq!(m.add(Matrix::identity()), 0);
        assert_eq!(m.add(Matrix::identity()), 1);
    }

    #[test]
    fn remove_prefers_entries_over_simple_entries() {
        let mut m = Minimap::default();
        let full = m.add(Matrix::identity());
        let simple = m.add_simple(Matrix::identity(), Vector4::new(0.0, 0.0, 0.0, 0.0));
        m.remove(full);
        assert!(m.entries().is_empty());
        assert_eq!(m.simple_entries().len(), 1);
        m.remove(simple);
        assert!(m.simple_entries().is_empty());
        // Removing an unknown handle is a no-op.
        m.remove(9999);
    }

    #[test]
    fn colour_packing_is_argb_with_clamping() {
        let packed = colour_to_u32(Vector4::new(300.0, -5.0, 12.7, 255.0));
        assert_eq!(packed, (255u32 << 24) | (255 << 16) | 12);
    }

    #[test]
    fn texture_layers_draw_in_key_order_and_delete_by_key() {
        let mut m = Minimap::default();
        let layer = |n: f32| TextureLayer {
            world_size: Vector2 { x: n, y: n },
            world_anchor: Vector2 { x: 0.0, y: 0.0 },
            clip_region: Vector4::new(0.0, 0.0, 0.0, 0.0),
        };
        m.add_texture_layer(5, layer(1.0));
        m.add_texture_layer(2, layer(2.0));
        m.add_texture_layer(5, layer(3.0));
        let order: Vec<u32> = m
            .update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor())
            .clip_regions
            .iter()
            .map(|(key, _)| *key)
            .collect();
        // Key order, equal keys in insertion order (the multimap).
        assert_eq!(order, vec![2, 5, 5]);
        m.del_texture_layer(5);
        let order: Vec<u32> = m
            .update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor())
            .clip_regions
            .iter()
            .map(|(key, _)| *key)
            .collect();
        assert_eq!(order, vec![2]);
        m.del_all_texture_layers();
        assert!(
            m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor())
                .clip_regions
                .is_empty()
        );
    }

    #[test]
    fn zoom_interpolation_clamps_the_amount() {
        let mut m = Minimap {
            range: 1000.0,
            ..Default::default()
        };
        // dTime/zoomSpeed = 0.0025/0.25 = 0.01 — the floor.
        let frame = m.update(&Matrix::identity(), 0.0025, 512.0, 384.0, &hidden_cursor());
        assert!(close(frame.current_range, 505.0));
        // A long frame clamps to 1: the range is reached exactly.
        let frame = m.update(&Matrix::identity(), 1000.0, 512.0, 384.0, &hidden_cursor());
        assert!(close(frame.current_range, 1000.0));
        assert!(close(m.current_range, 1000.0));
    }

    #[test]
    fn update_reports_camera_and_the_aspect_sandwich() {
        let mut m = Minimap::default();
        let cam = yawed(std::f32::consts::FRAC_PI_2, 100.0, 200.0);
        let frame = m.update(&cam, 0.0, 512.0, 384.0, &hidden_cursor());
        assert!(close3(frame.cam_pos, v3(100.0, 0.0, 200.0)));
        assert!(close(frame.cam_yaw, std::f32::consts::FRAC_PI_2));
        // relRot = setRotateZ(yaw) pre-multiplied by scale(h/w) and
        // post-multiplied by scale(w/h); with column applyPoint the
        // stored product applies scale(h/w), then the rotation, then
        // scale(w/h) — clip space is squared, rotated, stretched back.
        let out = frame.transform.apply_point(v3(2.0, 3.0, 0.0));
        let aspect = 1024.0 / 768.0;
        assert!(close(out.x, -3.0 / aspect));
        assert!(close(out.y, 2.0 * aspect));
        assert!(close(out.z, 0.0));
    }

    #[test]
    fn simple_entries_project_to_half_scale_with_z_half() {
        let mut m = Minimap::default();
        let handle = m.add_simple(
            translation(10.0, 0.0, 20.0),
            Vector4::new(255.0, 0.0, 0.0, 128.0),
        );
        let frame = m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor());
        assert_eq!(frame.simple_entries.len(), 1);
        let (got, vertex) = &frame.simple_entries[0];
        assert_eq!(*got, handle);
        assert!(close3(vertex.position, v3(0.01, 0.02, 0.5)));
        assert_eq!(vertex.uv, Vector2 { x: 0.0, y: 0.0 });
        assert_eq!(vertex.colour, (128u32 << 24) | (255 << 16));
    }

    #[test]
    fn simple_entries_outside_the_margin_are_hidden() {
        let mut m = Minimap::default();
        // 494/500 = 0.988 >= 1 - 0.05: hidden (the strict `<` gate).
        m.add_simple(
            translation(494.0, 0.0, 0.0),
            Vector4::new(1.0, 1.0, 1.0, 1.0),
        );
        // 474/500 = 0.948 < 0.95: visible.
        m.add_simple(
            translation(474.0, 0.0, 0.0),
            Vector4::new(1.0, 1.0, 1.0, 1.0),
        );
        let frame = m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor());
        assert_eq!(frame.simple_entries.len(), 1);
        assert!(close3(
            frame.simple_entries[0].1.position,
            v3(0.474, 0.0, 0.5)
        ));
    }

    #[test]
    fn entries_project_with_translation_and_counter_yaw() {
        let mut m = Minimap::default();
        let handle = m.add(yawed(std::f32::consts::FRAC_PI_2, 100.0, 0.0));
        let frame = m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor());
        assert_eq!(frame.entries.len(), 1);
        let (got, projection) = &frame.entries[0];
        assert_eq!(*got, handle);
        assert!(projection.visible);
        assert!(close3(projection.position, v3(0.1, 0.0, 0.0)));
        // The marker transform is the counter-rotated, aspect-sandwiched
        // rotation about the projected translation (scale(h/w), then
        // rotateZ(-yaw), then scale(w/h)).
        let aspect = 1024.0 / 768.0;
        let out = projection.transform.apply_point(v3(1.0, 2.0, 0.0));
        // The counter-rotation applies first, then the projected
        // translation.
        assert!(close(out.x, 2.0 / aspect + 0.1));
        assert!(close(out.y, -aspect));
        assert!(close(out.z, 0.0));
        // And the translation survives the sandwich.
        assert!(close3(
            projection.transform.apply_to_origin(),
            v3(0.1, 0.0, 0.0)
        ));
    }

    #[test]
    fn entries_beyond_the_range_keep_their_matrix_and_hide() {
        let mut m = Minimap::default();
        let original = yawed(std::f32::consts::FRAC_PI_2, 600.0, 0.0);
        let handle = m.add(original);
        let frame = m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor());
        let projection = &frame.entries[0].1;
        assert!(!projection.visible);
        // The matrix is untouched while out of range.
        assert_eq!(projection.transform, original);
        assert!(close3(projection.position, v3(600.0, 0.0, 0.0)));
        let _ = handle;
    }

    #[test]
    fn entry_at_the_margin_boundary_stays_visible() {
        let mut m = Minimap::default();
        // 475 == 500·(1-0.05): the `<=` gate keeps it.
        m.add(translation(475.0, 0.0, 0.0));
        let frame = m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor());
        assert!(frame.entries[0].1.visible);
        assert!(close3(frame.entries[0].1.position, v3(0.475, 0.0, 0.0)));
    }

    #[test]
    fn clip_regions_map_the_world_anchor_and_size() {
        let mut m = Minimap::default();
        m.add_texture_layer(
            1,
            TextureLayer {
                world_size: Vector2 { x: 200.0, y: 100.0 },
                world_anchor: Vector2 { x: 100.0, y: 200.0 },
                clip_region: Vector4::new(0.0, 0.0, 0.0, 0.0),
            },
        );
        let frame = m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor());
        let region = frame.clip_regions[0].1;
        // anchor-relative corners scaled by 1/500 · half-width + pos
        // (legacy pos (0.5, -0.5), half size (0.5, 0.5)).
        assert!(close(region.x, 0.5));
        assert!(close(region.y, -0.25));
        assert!(close(region.z, 0.7));
        assert!(close(region.w, -0.35));
    }

    #[test]
    fn negative_world_height_does_not_flip_the_region() {
        let mut m = Minimap::default();
        m.add_texture_layer(
            1,
            TextureLayer {
                world_size: Vector2 {
                    x: 200.0,
                    y: -100.0,
                },
                world_anchor: Vector2 { x: 100.0, y: 200.0 },
                clip_region: Vector4::new(0.0, 0.0, 0.0, 0.0),
            },
        );
        let frame = m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor());
        let region = frame.clip_regions[0].1;
        // Same region as the positive-height case: top stays above
        // bottom.
        assert!(close(region.x, 0.5));
        assert!(close(region.y, -0.25));
        assert!(close(region.z, 0.7));
        assert!(close(region.w, -0.35));
    }

    #[test]
    fn clip_regions_follow_the_camera() {
        let mut m = Minimap::default();
        m.add_texture_layer(
            1,
            TextureLayer {
                world_size: Vector2 { x: 200.0, y: 100.0 },
                world_anchor: Vector2 { x: 100.0, y: 200.0 },
                clip_region: Vector4::new(0.0, 0.0, 0.0, 0.0),
            },
        );
        let cam = translation(100.0, 0.0, 200.0);
        let frame = m.update(&cam, 0.0, 512.0, 384.0, &hidden_cursor());
        let region = frame.clip_regions[0].1;
        // The anchor sits on the camera: the region is centred on the
        // minimap position — (anchor±size/2)/500·half + pos.
        assert!(close(region.x, 0.5 - 0.1));
        assert!(close(region.y, -0.5 + 0.05));
        assert!(close(region.z, 0.5 + 0.1));
        assert!(close(region.w, -0.5 - 0.05));
    }

    #[test]
    fn aabb_clip_passes_a_fully_inside_segment() {
        let box6 = Aabb::new(v3(-1.0, -1.0, -1.0), v3(1.0, 1.0, 1.0));
        let mut source = v3(-0.5, 0.25, -0.75);
        let mut extent = v3(0.5, 0.5, 0.25);
        assert!(box6.clip(&mut source, &mut extent, 0.0));
        assert!(close3(source, v3(-0.5, 0.25, -0.75)));
        assert!(close3(extent, v3(0.5, 0.5, 0.25)));
    }

    #[test]
    fn aabb_clip_trims_a_straddling_segment_both_directions() {
        let box6 = Aabb::new(v3(-1.0, -1.0, -1.0), v3(1.0, 1.0, 1.0));
        let mut source = v3(-3.0, 0.5, 0.5);
        let mut extent = v3(3.0, 0.5, 0.5);
        assert!(box6.clip(&mut source, &mut extent, 0.0));
        assert!(close3(source, v3(-1.0, 0.5, 0.5)));
        assert!(close3(extent, v3(1.0, 0.5, 0.5)));
        // The reversed segment picks the same faces.
        let mut source = v3(3.0, 0.5, 0.5);
        let mut extent = v3(-3.0, 0.5, 0.5);
        assert!(box6.clip(&mut source, &mut extent, 0.0));
        assert!(close3(source, v3(1.0, 0.5, 0.5)));
        assert!(close3(extent, v3(-1.0, 0.5, 0.5)));
    }

    #[test]
    fn aabb_clip_rejects_a_degenerate_axis_outside() {
        let box6 = Aabb::new(v3(-1.0, -1.0, -1.0), v3(1.0, 1.0, 1.0));
        // Constant x inside: the y span is clipped instead.
        let mut source = v3(0.5, -3.0, 0.0);
        let mut extent = v3(0.5, 3.0, 0.0);
        assert!(box6.clip(&mut source, &mut extent, 0.0));
        assert!(close3(source, v3(0.5, -1.0, 0.0)));
        // Constant x outside: rejected (`source >= max + bloat` is the
        // upper rejection edge, so 1.5 with bloat 0.5 is already out).
        // The bloat widens the band: 1.4 passes with bloat 0.5 but not
        // with bloat 0.
        let mut source = v3(2.0, -3.0, 0.0);
        let mut extent = v3(2.0, 3.0, 0.0);
        assert!(!box6.clip(&mut source, &mut extent, 0.0));
        let mut source = v3(1.4, -3.0, 0.0);
        let mut extent = v3(1.4, 3.0, 0.0);
        assert!(!box6.clip(&mut source, &mut extent, 0.0));
        let mut source = v3(1.4, -3.0, 0.0);
        let mut extent = v3(1.4, 3.0, 0.0);
        assert!(box6.clip(&mut source, &mut extent, 0.5));
        let mut source = v3(1.5, -3.0, 0.0);
        let mut extent = v3(1.5, 3.0, 0.0);
        assert!(!box6.clip(&mut source, &mut extent, 0.5));
    }

    #[test]
    fn aabb_clip_trims_a_z_straddling_segment_both_directions() {
        let box6 = Aabb::new(v3(-1.0, -1.0, -1.0), v3(1.0, 1.0, 1.0));
        let mut source = v3(0.5, 0.25, -3.0);
        let mut extent = v3(0.5, 0.25, 3.0);
        assert!(box6.clip(&mut source, &mut extent, 0.0));
        assert!(close3(source, v3(0.5, 0.25, -1.0)));
        assert!(close3(extent, v3(0.5, 0.25, 1.0)));
        // The reversed segment picks the same faces.
        let mut source = v3(0.5, 0.25, 3.0);
        let mut extent = v3(0.5, 0.25, -3.0);
        assert!(box6.clip(&mut source, &mut extent, 0.0));
        assert!(close3(source, v3(0.5, 0.25, 1.0)));
        assert!(close3(extent, v3(0.5, 0.25, -1.0)));
    }

    #[test]
    fn aabb_clip_handles_negative_y_and_out_of_band_degenerates() {
        let box6 = Aabb::new(v3(-1.0, -1.0, -1.0), v3(1.0, 1.0, 1.0));
        // A segment descending through the box: with delta.y < 0 the max
        // face is the entry and the min face the exit.
        let mut source = v3(0.0, 3.0, 0.5);
        let mut extent = v3(0.0, -3.0, 0.5);
        assert!(box6.clip(&mut source, &mut extent, 0.0));
        assert!(close3(source, v3(0.0, 1.0, 0.5)));
        assert!(close3(extent, v3(0.0, -1.0, 0.5)));
        // Constant y above the box: rejected by the degenerate y test.
        let mut source = v3(0.0, 2.0, -3.0);
        let mut extent = v3(0.0, 2.0, 3.0);
        assert!(!box6.clip(&mut source, &mut extent, 0.0));
        // Constant z beyond it: rejected by the degenerate z test.
        let mut source = v3(0.0, -3.0, 2.0);
        let mut extent = v3(0.0, 3.0, 2.0);
        assert!(!box6.clip(&mut source, &mut extent, 0.0));
    }

    #[test]
    fn aabb_clip_rejects_when_the_slabs_disagree() {
        let box6 = Aabb::new(v3(-1.0, -1.0, -1.0), v3(1.0, 1.0, 1.0));
        // Line x + y = 3 misses the corner: the y slab's entry is past
        // the x slab's exit.
        let mut source = v3(1.0, 2.0, 0.0);
        let mut extent = v3(2.0, 1.0, 0.0);
        assert!(!box6.clip(&mut source, &mut extent, 0.0));
    }

    #[test]
    fn distance_to_entry_measures_in_range_entries_only() {
        let m = Minimap::default();
        let near = translation(2.0, 0.0, 4.0);
        let far = translation(490.0, 0.0, 0.0); // 0.98 >= 0.95: out.
        let mpos = Vector2 { x: 0.0, y: 0.0 };
        let d = m.distance_to_entry(&mpos, &v3(0.0, 0.0, 0.0), &near);
        assert!(close(d, (0.004f32).hypot(0.008))); // (2, 4)/500, y <- z.
        let d = m.distance_to_entry(&mpos, &v3(0.0, 0.0, 0.0), &far);
        assert_eq!(d, FLT_MAX);
    }

    #[test]
    fn mouse_pick_records_blur_and_focus_in_order() {
        let mut m = Minimap {
            mouse_entry_picking: true,
            ..Default::default()
        };
        let script = MinimapCallbacks::new();
        m.set_script(script.clone());
        // rel (0.004, 0) and (0, 0.004); entrySize = 10/1024 = 0.0098.
        m.add_simple(translation(2.0, 0.0, 0.0), Vector4::new(1.0, 1.0, 1.0, 1.0));
        m.add_simple(translation(0.0, 0.0, 2.0), Vector4::new(1.0, 1.0, 1.0, 1.0));
        let cam_pos = v3(0.0, 0.0, 0.0);
        let center = v3(0.0, 0.0, 0.0);
        let size = Vector2 { x: 2.0, y: 2.0 };

        m.do_mouse_pick(&cam_pos, &center, &size, &cursor_at(0.004, 0.0));
        m.do_mouse_pick(&cam_pos, &center, &size, &cursor_at(0.0, 0.004));
        // Off both markers but inside the margin: blur, no focus.
        m.do_mouse_pick(&cam_pos, &center, &size, &cursor_at(0.5, 0.0));
        // Nothing picked and still nothing near: no calls at all.
        m.do_mouse_pick(&cam_pos, &center, &size, &cursor_at(0.9, 0.9));
        // The margin gate blurs (no-op when nothing is picked).
        m.do_mouse_pick(&cam_pos, &center, &size, &cursor_at(0.95, 0.0));
        // An invisible cursor blurs too.
        m.do_mouse_pick(&cam_pos, &center, &size, &hidden_cursor());

        assert_eq!(
            script.calls(),
            vec![
                ("onEntryFocus".to_string(), 2),
                ("onEntryBlur".to_string(), 2),
                ("onEntryFocus".to_string(), 3),
                ("onEntryBlur".to_string(), 3),
            ]
        );
        assert_eq!(m.picked_entry(), 0);
    }

    #[test]
    fn mouse_pick_gates_on_the_entry_size_threshold() {
        let mut m = Minimap {
            mouse_entry_picking: true,
            ..Default::default()
        };
        let script = MinimapCallbacks::new();
        m.set_script(script.clone());
        // rel (0.01, 0); a cursor 0.04 away exceeds entrySize
        // 10/1024/1 = 0.0098, so the nearest candidate loses to the
        // threshold.
        m.add_simple(translation(5.0, 0.0, 0.0), Vector4::new(1.0, 1.0, 1.0, 1.0));
        let cam_pos = v3(0.0, 0.0, 0.0);
        let center = v3(0.0, 0.0, 0.0);
        let size = Vector2 { x: 2.0, y: 2.0 };
        m.do_mouse_pick(&cam_pos, &center, &size, &cursor_at(0.05, 0.0));
        assert!(script.calls().is_empty());

        // A big enough entry size admits the same cursor.
        m.simple_entry_size = 100.0; // 100/1024/1 = 0.0977 > 0.04.
        m.do_mouse_pick(&cam_pos, &center, &size, &cursor_at(0.05, 0.0));
        assert_eq!(script.calls(), vec![("onEntryFocus".to_string(), 2)]);
    }

    #[test]
    fn the_pick_uses_the_previous_frame_transform() {
        let mut m = Minimap {
            mouse_entry_picking: true,
            ..Default::default()
        };
        let script = MinimapCallbacks::new();
        m.set_script(script.clone());
        // A marker 0.002 clip units from the camera.
        m.add_simple(translation(1.0, 0.0, 0.0), Vector4::new(1.0, 1.0, 1.0, 1.0));

        // Frame 1 rotates the minimap by yaw π/4; its pick runs with the
        // identity transform and an invisible cursor (no calls).
        let cam = yawed(std::f32::consts::FRAC_PI_4, 0.0, 0.0);
        m.update(&cam, 0.0, 512.0, 384.0, &hidden_cursor());
        assert!(script.calls().is_empty());

        // Frame 2's pick centre is the quad centre transformed by frame
        // 1's runTimeTransform: with column applyPoint the transformed
        // (1, 0) reads off the matrix as (m00, m01) = (cos, (h/w)·sin).
        let c = std::f32::consts::FRAC_1_SQRT_2;
        let aspect = 1024.0 / 768.0;
        let centre_x = c;
        let centre_y = c * aspect;
        m.update(
            &Matrix::identity(),
            0.0,
            512.0,
            384.0,
            &cursor_at(centre_x, centre_y),
        );
        assert_eq!(script.calls(), vec![("onEntryFocus".to_string(), 2)]);
    }

    #[test]
    fn cell_boundaries_emit_four_clipped_edges_with_uv_and_colour() {
        let mut m = Minimap {
            cell_bounds: vec![0.0, 0.0, 100.0, 100.0],
            ..Default::default()
        };
        let frame = m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor());
        assert_eq!(frame.cell_vertices.len(), 8);
        // Edge order: bottom, left, top, right; two vertices each.
        assert!(close3(frame.cell_vertices[0].position, v3(0.0, 0.0, 0.0)));
        assert!(close2(
            frame.cell_vertices[0].uv,
            Vector2 { x: 0.5, y: 0.5 }
        ));
        assert!(close3(frame.cell_vertices[1].position, v3(0.1, 0.0, 0.0)));
        assert!(close2(
            frame.cell_vertices[1].uv,
            Vector2 { x: 0.6, y: 0.5 }
        ));
        assert!(close3(frame.cell_vertices[2].position, v3(0.0, 0.0, 0.0)));
        assert!(close3(frame.cell_vertices[3].position, v3(0.0, 0.1, 0.0)));
        assert!(close3(frame.cell_vertices[4].position, v3(0.0, 0.1, 0.0)));
        assert!(close3(frame.cell_vertices[5].position, v3(0.1, 0.1, 0.0)));
        assert!(close3(frame.cell_vertices[6].position, v3(0.1, 0.0, 0.0)));
        assert!(close3(frame.cell_vertices[7].position, v3(0.1, 0.1, 0.0)));
        for vertex in &frame.cell_vertices {
            assert_eq!(vertex.colour, 0x8080_8080);
        }
    }

    #[test]
    fn cell_boundaries_trim_to_the_visible_box() {
        let mut m = Minimap {
            // A cell overhanging the visible box: the horizontal edges
            // trim to ±currentRange, the vertical edges sit on
            // degenerate outside x and are rejected.
            cell_bounds: vec![-1000.0, 0.0, 1000.0, 100.0],
            ..Default::default()
        };
        let frame = m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor());
        assert_eq!(frame.cell_vertices.len(), 4);
        assert!(close3(frame.cell_vertices[0].position, v3(-0.5, 0.0, 0.0)));
        assert!(close3(frame.cell_vertices[1].position, v3(0.5, 0.0, 0.0)));
        assert!(close3(frame.cell_vertices[2].position, v3(-0.5, 0.1, 0.0)));
        assert!(close3(frame.cell_vertices[3].position, v3(0.5, 0.1, 0.0)));
    }

    #[test]
    fn cell_boundaries_are_clamped_at_cell_max_value() {
        let mut m = Minimap {
            // bl.x/tr.x of 200000 exceed CELL_MAX_VALUE; clamped to
            // 100000 the cell's vertical edge is INSIDE a 200000 range
            // (without the clamp it would sit on the rejection boundary
            // and vanish).
            cell_bounds: vec![200000.0, 0.0, 200000.0, 100.0],
            range: 200000.0,
            ..Default::default()
        };
        // One full zoom step: dTime == zoomSpeed.
        let frame = m.update(&Matrix::identity(), 0.25, 512.0, 384.0, &hidden_cursor());
        assert!(close(frame.current_range, 200000.0));
        assert_eq!(frame.cell_vertices.len(), 8);
        // First vertex: 100000/200000·0.5, proving the clamp.
        assert!(close3(frame.cell_vertices[0].position, v3(0.25, 0.0, 0.0)));
    }

    #[test]
    fn hidden_layers_and_boundaries_emit_nothing() {
        let mut m = Minimap {
            simple_entries_visible: false,
            cell_bounds_visible: false,
            cell_bounds: vec![0.0, 0.0, 1.0, 1.0],
            ..Default::default()
        };
        m.add_simple(translation(1.0, 0.0, 0.0), Vector4::new(1.0, 1.0, 1.0, 1.0));
        let frame = m.update(&Matrix::identity(), 0.0, 512.0, 384.0, &hidden_cursor());
        assert!(frame.simple_entries.is_empty());
        assert!(frame.cell_vertices.is_empty());
    }

    #[test]
    fn point_size_scales_with_the_range_when_configured() {
        let mut m = Minimap::default();
        assert!(close(m.point_size(), 10.0));
        m.point_size_scaling = Vector2 { x: 2.0, y: 0.1 };
        assert!(close(m.point_size(), 2.0 + 0.1 * 500.0));
        // 0.0004 is NOT almost zero (strict <): the formula wins.
        m.point_size_scaling = Vector2 { x: 0.0004, y: 0.0 };
        assert!(close(m.point_size(), 0.0004));
        m.point_size_scaling = Vector2 { x: 0.00039, y: 0.0 };
        assert!(close(m.point_size(), 10.0));
    }

    #[test]
    fn the_border_takes_the_rotation_when_rotate_is_off() {
        let mut m = Minimap {
            rotate: false,
            ..Default::default()
        };
        let cam = yawed(std::f32::consts::FRAC_PI_2, 0.0, 0.0);
        let frame = m.update(&cam, 0.0, 512.0, 384.0, &hidden_cursor());
        assert_eq!(frame.transform, Matrix::identity());
        let border = frame.border_transform.expect("border transform");
        let aspect = 1024.0 / 768.0;
        let out = border.apply_point(v3(2.0, 3.0, 0.0));
        assert!(close(out.x, -3.0 / aspect));
        assert!(close(out.y, 2.0 * aspect));
        // And the next frame's pick keeps using the identity transform:
        // the quad centre stays (1, 0) for the legacy square.
        m.mouse_entry_picking = true;
        let script = MinimapCallbacks::new();
        m.set_script(script.clone());
        m.add_simple(translation(1.0, 0.0, 0.0), Vector4::new(1.0, 1.0, 1.0, 1.0));
        m.update(&cam, 0.0, 512.0, 384.0, &cursor_at(1.0, 0.0));
        assert_eq!(script.calls(), vec![("onEntryFocus".to_string(), 2)]);
    }
}
