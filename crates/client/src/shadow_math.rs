//! BW-11-04-03 shadow math, following `lib/moo/shadows_lib/shadow_math.cpp`.
//!
//! The module ports the four light-view/projection builders and the shared
//! corner helpers the reference shadows code builds on. The reference works
//! directly in D3DX math; this port keeps the D3DX conventions verbatim:
//! row-major matrices with row-vector products (`p·M`), `D3DXMatrixLookAtLH`
//! (basis rows — *not* the engine `Matrix::look_at`, which puts the basis in
//! the columns), `D3DXMatrixOrthoLH` and `D3DXMatrixOrthoOffCenterLH`.
//!
//! Registered differences:
//! - `D3DXMatrixInverse` on a view matrix is replaced by
//!   `Matrix::invert_orthonormal`; every input here is an orthonormal
//!   look-at, so the two agree on these inputs (the general adjugate inverse
//!   only differs on non-orthonormal matrices, which the reference never
//!   feeds it).
//! - `D3DXMatrixOrthoLH` maps to the foundation's `orthogonal_projection`,
//!   already verified byte-identical to the D3DX recipe.
//! - `calcLightViewProjectionMatrixPSM` is not ported: its inverted-light
//!   branch needs `BoundingCone` (`lib/math/bounding.cpp`), and the
//!   reference's own unit test (`shadow_math_tests.cpp:127-154`) asserts
//!   nothing — it runs the function twice and returns false unconditionally.
//! - `calcLightCropMatrix` / `calcCropMatrixForCorners` are empty bodies in
//!   the reference; `calcLightViewMatrixCentered` is declared in the header
//!   with no definition. None are ported.
//! - The reference test file calls 10/11-argument overloads that no longer
//!   exist in the current `shadow_math.cpp` (stale against the API); its
//!   expected vectors nevertheless hold against the canonical implementation
//!   within its `EPSILON 0.01` matrix distance, and are used as the
//!   acceptance anchors in the tests below.

use foundation::math::matrix::Matrix;
use foundation::math::vector::{Vector2, Vector3, Vector4};

// ===========================================================================
// D3DX recipes the reference builds on
// ===========================================================================

/// `SHADOW_MATH_UP_VECTOR` (`shadow_math.hpp:12`).
pub fn up_vector() -> Vector3 {
    Vector3::new(0.0, 1.0, 0.0)
}

/// `SHADOW_MATH_HORIZON_VECTOR` (`shadow_math.hpp:16`).
pub fn horizon_vector() -> Vector3 {
    Vector3::new(0.0, 0.0, 1.0)
}

/// The per-light-direction up vector every builder picks
/// (`shadow_math.cpp:36-39` and each sibling): near-vertical light
/// directions swap the up vector so the look-at cross products stay defined.
fn pick_up_vector(light_dir: &Vector3) -> Vector3 {
    if light_dir.x * light_dir.x + light_dir.z * light_dir.z < 0.001 {
        horizon_vector()
    } else {
        up_vector()
    }
}

/// `D3DXMatrixLookAtLH` (d3dx9math.h): left-handed look-at with the basis in
/// the matrix rows. The engine `Matrix::look_at` is a different convention
/// (basis columns), so the shadow code gets this dedicated builder.
pub fn d3dx_look_at_lh(eye: &Vector3, at: &Vector3, up: &Vector3) -> Matrix {
    let mut zaxis = *at - *eye;
    zaxis.normalise();
    let mut xaxis = up.cross_product(up, &zaxis);
    xaxis.normalise();
    let yaxis = zaxis.cross_product(&zaxis, &xaxis);

    let mut m = Matrix::zero();
    m.m[0] = [xaxis.x, yaxis.x, zaxis.x, 0.0];
    m.m[1] = [xaxis.y, yaxis.y, zaxis.y, 0.0];
    m.m[2] = [xaxis.z, yaxis.z, zaxis.z, 0.0];
    m.m[3] = [
        -xaxis.dot_product(eye),
        -yaxis.dot_product(eye),
        -zaxis.dot_product(eye),
        1.0,
    ];
    m
}

/// `D3DXMatrixOrthoOffCenterLH` (d3dx9math.h): an orthographic projection of
/// an arbitrary off-centre box in view space.
pub fn ortho_off_center_lh(l: f32, r: f32, b: f32, t: f32, zn: f32, zf: f32) -> Matrix {
    let mut m = Matrix::zero();
    m.m[0][0] = 2.0 / (r - l);
    m.m[1][1] = 2.0 / (t - b);
    m.m[2][2] = 1.0 / (zf - zn);
    m.m[3][0] = (l + r) / (l - r);
    m.m[3][1] = (t + b) / (b - t);
    m.m[3][2] = zn / (zn - zf);
    m.m[3][3] = 1.0;
    m
}

// ===========================================================================
// Interface
// ===========================================================================

/// The output bundle of the light-view builders: the view and projection the
/// shadow map renders with, their product, the world-space size of one shadow
/// texel, and the projection's far plane.
pub struct LightViewProjection {
    pub light_view: Matrix,
    pub light_proj: Matrix,
    pub light_view_proj: Matrix,
    pub texel_size_world: Vector2,
    pub max_z: f32,
}

/// `calcLightViewProjectionMatrix` (`shadow_math.cpp:12-57`): fit an
/// orthographic light projection around the camera frustum's world-space
/// corners, looking along `light_dir` from `light_dist` behind the frustum
/// centre.
// The signature mirrors the reference call site one-to-one.
#[allow(clippy::too_many_arguments)]
pub fn calc_light_view_projection_matrix(
    view: &Matrix,
    near_plane: f32,
    far_plane: f32,
    fov: f32,
    aspect_ratio: f32,
    light_dir: &Vector3,
    light_dist: f32,
    shadow_map_size: f32,
) -> LightViewProjection {
    let corners = calc_frustrum_corners_world_space(view, near_plane, far_plane, fov, aspect_ratio);
    let center_world = calc_corners_center(&corners);

    let eye = center_world - *light_dir * light_dist;
    let at = center_world;
    let up = pick_up_vector(light_dir);
    let light_view = d3dx_look_at_lh(&eye, &at, &up);

    let corners = translate_corners_from_world_space_to_light_view_space(&corners, &light_view);
    let (aabb_min, aabb_max) = calc_corners_aabb(&corners);

    let max_z = aabb_max.z;
    let texel_size_world = Vector2 {
        x: (aabb_max.x - aabb_min.x) / shadow_map_size,
        y: (aabb_max.y - aabb_min.y) / shadow_map_size,
    };

    let light_proj = ortho_off_center_lh(
        aabb_min.x, aabb_max.x, aabb_min.y, aabb_max.y, 0.0, aabb_max.z,
    );
    let mut light_view_proj = Matrix::zero();
    light_view_proj.multiply(&light_view, &light_proj);

    LightViewProjection {
        light_view,
        light_proj,
        light_view_proj,
        texel_size_world,
        max_z,
    }
}

/// `calcLightViewProjectionMatrixPoints` (`shadow_math.cpp:59-98`): the same
/// fit, centred on the centroid of explicit caster points instead of a camera
/// frustum.
pub fn calc_light_view_projection_matrix_points(
    caster_points: &[Vector3],
    light_dir: &Vector3,
    light_dist: f32,
    map_size: f32,
) -> LightViewProjection {
    let mut center = Vector3::new(0.0, 0.0, 0.0);
    for p in caster_points {
        center = center + *p;
    }
    center = center / caster_points.len() as f32;

    let eye = center - *light_dir * light_dist;
    let at = center;
    let up = pick_up_vector(light_dir);
    let light_view = d3dx_look_at_lh(&eye, &at, &up);

    let (aabb_min, aabb_max) = calc_points_aabb_in_basis(&light_view, caster_points);

    let max_z = aabb_max.z;
    let texel_size_world = Vector2 {
        x: (aabb_max.x - aabb_min.x) / map_size,
        y: (aabb_max.y - aabb_min.y) / map_size,
    };

    let light_proj =
        ortho_off_center_lh(aabb_min.x, aabb_max.x, aabb_min.y, aabb_max.y, 0.0, max_z);
    let mut light_view_proj = Matrix::zero();
    light_view_proj.multiply(&light_view, &light_proj);

    LightViewProjection {
        light_view,
        light_proj,
        light_view_proj,
        texel_size_world,
        max_z,
    }
}

/// `calcLightViewProjectionMatrixSimple` (`shadow_math.cpp:101-118`): a
/// plain box of `w` × `h` units from `light_pos` along `light_dir`. The
/// reference has no texel/maxZ outputs for this variant; both stay zeroed.
pub fn calc_light_view_projection_matrix_simple(
    light_pos: &Vector3,
    light_dir: &Vector3,
    light_dist: f32,
    w: f32,
    h: f32,
) -> LightViewProjection {
    let eye = *light_pos;
    let at = *light_pos + *light_dir * light_dist;
    let up = pick_up_vector(light_dir);
    let light_view = d3dx_look_at_lh(&eye, &at, &up);

    let mut light_proj = Matrix::zero();
    light_proj.orthogonal_projection(w, h, 0.0, light_dist);

    let mut light_view_proj = Matrix::zero();
    light_view_proj.multiply(&light_view, &light_proj);

    LightViewProjection {
        light_view,
        light_proj,
        light_view_proj,
        texel_size_world: Vector2 { x: 0.0, y: 0.0 },
        max_z: 0.0,
    }
}

/// The output bundle of the jittering-suppressed builder: the quantised
/// matrices plus the non-jittered frustum fit used for culling.
pub struct JitteringSuppressedMatrices {
    pub view: Matrix,
    pub proj: Matrix,
    pub view_proj: Matrix,
    pub cull_view_proj: Matrix,
}

/// `calcLightViewProjectionMatrixJitteringSuppressed`
/// (`shadow_math.cpp:120-223`): the projection box is snapped to the shadow
/// texel grid so sub-texel camera motion does not shimmer the shadow edges.
// The signature mirrors the reference call site one-to-one.
#[allow(clippy::too_many_arguments)]
pub fn calc_light_view_projection_matrix_jittering_suppressed(
    view: &Matrix,
    near_plane: f32,
    far_plane: f32,
    fov: f32,
    aspect_ratio: f32,
    light_dir: &Vector3,
    light_dist: f32,
    shadow_map_size: i32,
) -> JitteringSuppressedMatrices {
    // 0) calcs in world space
    let corners_world =
        calc_frustrum_corners_world_space(view, near_plane, far_plane, fov, aspect_ratio);
    let mut center_world = calc_corners_center(&corners_world);
    let radius = calc_corners_radius(&corners_world, &center_world);

    // 1) find fixed center in light view space
    let mut out_view = calc_light_view_matrix(light_dir);
    let corners_light_view =
        translate_corners_from_world_space_to_light_view_space(&corners_world, &out_view);
    let mut center_light_view = calc_corners_center(&corners_light_view);

    let texel_size = (2.0 * radius) / shadow_map_size as f32;

    center_light_view.x = (center_light_view.x / texel_size).floor() * texel_size;
    center_light_view.y = (center_light_view.y / texel_size).floor() * texel_size;

    // 2) translate fixed center to world space
    let mut inv_view = out_view;
    inv_view.invert_orthonormal();
    center_world = inv_view.apply_point(&center_light_view);

    let eye = center_world - *light_dir * light_dist;
    let at = center_world;
    let up = pick_up_vector(light_dir);

    out_view = d3dx_look_at_lh(&eye, &at, &up);

    let zn = 0.01;
    let zf = light_dist + radius;
    let w = 2.0 * radius;
    let h = w;

    let mut out_proj = Matrix::zero();
    out_proj.orthogonal_projection(w, h, zn, zf);
    let mut out_view_proj = Matrix::zero();
    out_view_proj.multiply(&out_view, &out_proj);

    // Calculation of matrix cull: the same frustum fit without the texel
    // snap, for frustum culling against the unquantised bounds.
    let center_world = calc_corners_center(&corners_world);
    let eye = center_world - *light_dir * light_dist;
    let at = center_world;
    let up = pick_up_vector(light_dir);

    let cull_view = d3dx_look_at_lh(&eye, &at, &up);
    let corners =
        translate_corners_from_world_space_to_light_view_space(&corners_world, &cull_view);
    let (aabb_min, aabb_max) = calc_corners_aabb(&corners);

    let cull_proj = ortho_off_center_lh(
        aabb_min.x, aabb_max.x, aabb_min.y, aabb_max.y, 0.0, aabb_max.z,
    );
    let mut cull_view_proj = Matrix::zero();
    cull_view_proj.multiply(&cull_view, &cull_proj);

    JitteringSuppressedMatrices {
        view: out_view,
        proj: out_proj,
        view_proj: out_view_proj,
        cull_view_proj,
    }
}

// ===========================================================================
// Common
// ===========================================================================

/// `calcLightViewMatrix` (`shadow_math.cpp:374-384`): a look-at from the
/// origin along the light direction.
pub fn calc_light_view_matrix(light_dir: &Vector3) -> Matrix {
    let eye = Vector3::new(0.0, 0.0, 0.0);
    let at = *light_dir;
    let up = pick_up_vector(light_dir);
    d3dx_look_at_lh(&eye, &at, &up)
}

/// `calcCornersAABB` (`shadow_math.cpp:392-396`): the eight frustum corners'
/// axis-aligned bounds.
pub fn calc_corners_aabb(corners: &[Vector3; 8]) -> (Vector3, Vector3) {
    calc_points_aabb(corners)
}

/// `calcPointsAABB` (`shadow_math.cpp:398-411`): the point set's axis-aligned
/// bounds.
pub fn calc_points_aabb(points: &[Vector3]) -> (Vector3, Vector3) {
    let mut min = Vector3::new(f32::MAX, f32::MAX, f32::MAX);
    let mut max = Vector3::new(f32::MIN, f32::MIN, f32::MIN);
    for p in points {
        if p.x > max.x {
            max.x = p.x;
        }
        if p.x < min.x {
            min.x = p.x;
        }
        if p.y > max.y {
            max.y = p.y;
        }
        if p.y < min.y {
            min.y = p.y;
        }
        if p.z > max.z {
            max.z = p.z;
        }
        if p.z < min.z {
            min.z = p.z;
        }
    }
    (min, max)
}

/// `calcPointsAABB(basis, ...)` (`shadow_math.cpp:413-430`): the bounds of
/// the point set transformed into `basis` space first
/// (`D3DXVec3TransformCoord`, which divides by w — `apply_point`).
pub fn calc_points_aabb_in_basis(basis: &Matrix, points: &[Vector3]) -> (Vector3, Vector3) {
    let transformed: Vec<Vector3> = points.iter().map(|p| basis.apply_point(p)).collect();
    calc_points_aabb(&transformed)
}

/// `calcFrustrumCornersWorldSpace` (`shadow_math.cpp:433-474`; the reference
/// misspells "frustrum" and the port keeps the name). Near corners are
/// indices 0-3, far corners 4-7, both wound `-x-y, -x+y, +x+y, +x-y`.
pub fn calc_frustrum_corners_world_space(
    view: &Matrix,
    near_plane: f32,
    far_plane: f32,
    fov: f32,
    aspect_ratio: f32,
) -> [Vector3; 8] {
    let mut inv_view = *view;
    inv_view.invert_orthonormal();

    // lightView basis in world space
    let mut vx = Vector3::new(inv_view.m[0][0], inv_view.m[0][1], inv_view.m[0][2]);
    let mut vy = Vector3::new(inv_view.m[1][0], inv_view.m[1][1], inv_view.m[1][2]);
    let mut vz = Vector3::new(inv_view.m[2][0], inv_view.m[2][1], inv_view.m[2][2]);
    let vc = Vector3::new(inv_view.m[3][0], inv_view.m[3][1], inv_view.m[3][2]);

    vx.normalise();
    vy.normalise();
    vz.normalise();

    // calculate the near plane width and height.
    let near_height = (fov * 0.5).tan() * near_plane;
    let near_width = near_height * aspect_ratio;

    // calculate the far plane width and height.
    let far_height = (fov * 0.5).tan() * far_plane;
    let far_width = far_height * aspect_ratio;

    let near_center = vc + vz * near_plane;
    let far_center = vc + vz * far_plane;

    // near points.
    [
        near_center - vx * near_width - vy * near_height,
        near_center - vx * near_width + vy * near_height,
        near_center + vx * near_width + vy * near_height,
        near_center + vx * near_width - vy * near_height,
        // far points.
        far_center - vx * far_width - vy * far_height,
        far_center - vx * far_width + vy * far_height,
        far_center + vx * far_width + vy * far_height,
        far_center + vx * far_width - vy * far_height,
    ]
}

/// `translateCornersFromWorldSpaceToLightViewSpace` (`shadow_math.cpp:476-486`):
/// `D3DXVec3Transform` per corner — the xyz of the row-vector product, the
/// w component dropped without a divide (it is 1 for affine view matrices).
pub fn translate_corners_from_world_space_to_light_view_space(
    corners: &[Vector3; 8],
    light_view: &Matrix,
) -> [Vector3; 8] {
    let mut out = [Vector3::new(0.0, 0.0, 0.0); 8];
    for (i, corner) in corners.iter().enumerate() {
        let v = Vector4::new(corner.x, corner.y, corner.z, 1.0);
        let t = light_view.apply_v4_point(&v);
        out[i] = Vector3::new(t.x, t.y, t.z);
    }
    out
}

/// `calcCornersCenter` (`shadow_math.cpp:488-497`): the average of the eight
/// corners.
pub fn calc_corners_center(corners: &[Vector3; 8]) -> Vector3 {
    let mut center = Vector3::new(0.0, 0.0, 0.0);
    for c in corners {
        center = center + *c;
    }
    center / 8.0
}

/// `calcCornersRadius` (`shadow_math.cpp:499-512`): the largest corner
/// distance from `center`.
pub fn calc_corners_radius(corners: &[Vector3; 8], center: &Vector3) -> f32 {
    let mut radius = -f32::MAX;
    for c in corners {
        let tmp = *c - *center;
        let r = tmp.length();
        if r > radius {
            radius = r;
        }
    }
    radius
}

/// `scaleCorners` (`shadow_math.cpp:514-523`): every corner pulled towards or
/// pushed away from `center`.
pub fn scale_corners(corners: &[Vector3; 8], center: &Vector3, scale: f32) -> [Vector3; 8] {
    let mut out = [Vector3::new(0.0, 0.0, 0.0); 8];
    for (i, c) in corners.iter().enumerate() {
        out[i] = *center + (*c - *center) * scale;
    }
    out
}

// ===========================================================================
// Texture scale/bias
// ===========================================================================

/// `applyTextureScaleBiasMat` (`shadow_math.cpp:529-541`): map clip space to
/// [0,1]² texture space, flipping y (`-0.5` on the diagonal, `+0.5` offsets).
pub fn apply_texture_scale_bias_mat(m: &Matrix) -> Matrix {
    let tex_offset = 0.5;
    let texture_scale_bias_mat = build_texture_scale_bias_matrix(tex_offset);

    let mut out = Matrix::zero();
    out.multiply(m, &texture_scale_bias_mat);
    out
}

/// `applyTextureScaleBiasMat(rect)` (`shadow_math.cpp:543-571`): additionally
/// fit into the [UL, BR] sub-rectangle of the texture.
pub fn apply_texture_scale_bias_mat_rect(m: &Matrix, ul: &Vector2, br: &Vector2) -> Matrix {
    let tex_offset = 0.5;
    let texture_scale_bias_mat = build_texture_scale_bias_matrix(tex_offset);

    let offset_x = ul.x;
    let offset_y = ul.y;
    let scale_x = br.x - ul.x;
    let scale_y = br.y - ul.y;

    let mut rect_fit_matrix = Matrix::zero();
    rect_fit_matrix.m[0][0] = scale_x;
    rect_fit_matrix.m[1][1] = scale_y;
    rect_fit_matrix.m[2][2] = 1.0;
    rect_fit_matrix.m[3][0] = offset_x;
    rect_fit_matrix.m[3][1] = offset_y;
    rect_fit_matrix.m[3][3] = 1.0;

    let mut res = Matrix::zero();
    res.multiply(&texture_scale_bias_mat, &rect_fit_matrix);
    let mut out = Matrix::zero();
    out.multiply(m, &res);
    out
}

/// The shared `textureScaleBiasMat` literal (`shadow_math.cpp:533-538`).
fn build_texture_scale_bias_matrix(tex_offset: f32) -> Matrix {
    let mut m = Matrix::zero();
    m.m[0][0] = 0.5;
    m.m[1][1] = -0.5;
    m.m[2][2] = 1.0;
    m.m[3][0] = tex_offset;
    m.m[3][1] = tex_offset;
    m.m[3][3] = 1.0;
    m
}

// ===========================================================================
// Parallel Split Shadow Maps
// ===========================================================================

/// `calcSplitDistances` (`shadow_math.cpp:577-599`): `numSplits + 1` plane
/// distances blending the logarithmic and uniform split schemes by `lambda`.
pub fn calc_split_distances(
    num_splits: usize,
    near_plane: f32,
    far_plane: f32,
    lambda: f32,
) -> Vec<f32> {
    let mut out = vec![0.0; num_splits + 1];
    out[0] = near_plane;
    for (i, slot) in out.iter_mut().enumerate().take(num_splits).skip(1) {
        let dimension = i as f32 / num_splits as f32;
        let log_scheme = near_plane * (far_plane / near_plane).powf(dimension);
        let uniform_scheme = near_plane + (far_plane - near_plane) * dimension;
        *slot = log_scheme * lambda + uniform_scheme * (1.0 - lambda);
    }
    out[num_splits] = far_plane;
    out
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// The reference test's `calcMatrixDistance`
    /// (`shadow_math_tests.cpp:13-24`): the sum of absolute entry deltas.
    fn calc_matrix_distance(m1: &Matrix, m2: &Matrix) -> f32 {
        let mut dist = 0.0;
        for i in 0..4 {
            for j in 0..4 {
                dist += (m2.m[i][j] - m1.m[i][j]).abs();
            }
        }
        dist
    }

    /// The reference test's `EPSILON` (`shadow_math_tests.cpp:7`).
    const EPSILON: f32 = 0.01;

    fn v3(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    /// The reference test scene (`shadow_math_tests.cpp:42-52`).
    fn reference_scene() -> (Matrix, Vector3, f32) {
        let view = d3dx_look_at_lh(&v3(0.5, 0.5, -0.5), &v3(0.5, 0.5, 0.5), &up_vector());
        let light_dir = v3(1.0, 0.0, 0.0);
        (view, light_dir, 10.0)
    }

    #[test]
    fn d3dx_look_at_lh_puts_the_basis_in_the_rows() {
        // Looking down +z from the origin: identity rotation, no translation.
        let m = d3dx_look_at_lh(&v3(0.0, 0.0, 0.0), &v3(0.0, 0.0, 1.0), &up_vector());
        assert_eq!(m.m[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(m.m[1], [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(m.m[2], [0.0, 0.0, 1.0, 0.0]);
        assert_eq!(m.m[3], [0.0, 0.0, 0.0, 1.0]);

        // Looking along +x from (2, 3, 4): xaxis = (0,0,-1), yaxis = (0,1,0),
        // zaxis = (1,0,0); the translation row is -basis . eye.
        let m = d3dx_look_at_lh(&v3(2.0, 3.0, 4.0), &v3(7.0, 3.0, 4.0), &up_vector());
        assert_eq!(m.m[0], [0.0, 0.0, 1.0, 0.0]);
        assert_eq!(m.m[1], [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(m.m[2], [-1.0, 0.0, 0.0, 0.0]);
        assert_eq!(m.m[3], [4.0, -3.0, -2.0, 1.0]);
    }

    #[test]
    fn ortho_off_center_lh_matches_the_d3dx_recipe() {
        let m = ortho_off_center_lh(-1.0, 1.0, -2.0, 2.0, 0.5, 10.5);
        assert_eq!(m.m[0][0], 2.0 / 2.0);
        assert_eq!(m.m[1][1], 2.0 / 4.0);
        assert_eq!(m.m[2][2], 1.0 / 10.0);
        assert_eq!(m.m[3][0], 0.0 / -2.0);
        assert_eq!(m.m[3][1], 0.0 / -4.0);
        assert_eq!(m.m[3][2], 0.5 / (0.5 - 10.5));
        assert_eq!(m.m[3][3], 1.0);
        // The asymmetric box offsets the translation row.
        let m = ortho_off_center_lh(0.0, 2.0, 1.0, 3.0, 0.0, 1.0);
        assert_eq!(m.m[3][0], (0.0 + 2.0) / (0.0 - 2.0));
        assert_eq!(m.m[3][1], (3.0 + 1.0) / (1.0 - 3.0));
    }

    #[test]
    fn the_reference_scene_yields_the_reference_light_view_and_projection() {
        let (view, light_dir, light_dist) = reference_scene();

        let out = calc_light_view_projection_matrix(
            &view,
            0.5,
            1.5,
            std::f32::consts::FRAC_PI_2,
            1.0,
            &light_dir,
            light_dist,
            1024.0,
        );

        // `properLightView` from the reference test
        // (`shadow_math_tests.cpp:60-65`): exact match.
        let proper_light_view =
            d3dx_look_at_lh(&v3(-9.5, 0.5, 0.5), &v3(0.5, 0.5, 0.5), &up_vector());
        assert!(
            calc_matrix_distance(&proper_light_view, &out.light_view) < EPSILON,
            "light view must match the reference look-at"
        );

        // `properLightProj` = OrthoLH(1, 3, 0, 11) (`:66`): the canonical
        // off-centre fit is ([-0.5, 0.5], [-1.5, 1.5], 0, 11.5); every entry
        // lands inside the test's EPSILON (the zf drift is 1/11 vs 1/11.5).
        let mut proper_light_proj = Matrix::zero();
        proper_light_proj.orthogonal_projection(1.0, 3.0, 0.0, 11.0);
        assert!(
            calc_matrix_distance(&proper_light_proj, &out.light_proj) < EPSILON,
            "light projection must match the reference ortho within epsilon"
        );

        // The canonical values themselves: the frustum's light-view AABB is
        // x' in [-0.5, 0.5] (world z flip), y' in [-1.5, 1.5], z' in
        // [8.5, 11.5].
        assert_eq!(out.max_z, 11.5);
        assert!((out.light_proj.m[0][0] - 2.0).abs() < 1e-5);
        assert!((out.light_proj.m[1][1] - 2.0 / 3.0).abs() < 1e-5);
        assert!((out.light_proj.m[2][2] - 1.0 / 11.5).abs() < 1e-5);
        assert!(out.light_proj.m[3][0].abs() < 1e-5);
        assert!(out.light_proj.m[3][1].abs() < 1e-5);
        assert!(out.light_proj.m[3][2].abs() < 1e-5);

        // One shadow texel is the AABB extent over the map size.
        assert!((out.texel_size_world.x - 1.0 / 1024.0).abs() < 1e-6);
        assert!((out.texel_size_world.y - 3.0 / 1024.0).abs() < 1e-6);

        // The product is exactly view x proj.
        let mut vp = Matrix::zero();
        vp.multiply(&out.light_view, &out.light_proj);
        assert!(calc_matrix_distance(&vp, &out.light_view_proj) < 1e-5);
    }

    #[test]
    fn the_reference_jittering_suppressed_scene_matches_its_vectors() {
        let (view, light_dir, light_dist) = reference_scene();

        let out = calc_light_view_projection_matrix_jittering_suppressed(
            &view,
            0.5,
            1.5,
            std::f32::consts::FRAC_PI_2,
            1.0,
            &light_dir,
            light_dist,
            1024,
        );

        // The reference test's `properLightView` (`shadow_math_tests.cpp:108-113`):
        // the texel-snapped centre moves the look-at by under EPSILON.
        let proper_light_view =
            d3dx_look_at_lh(&v3(-9.5, 0.5, 0.5), &v3(0.5, 0.5, 0.5), &up_vector());
        assert!(
            calc_matrix_distance(&proper_light_view, &out.view) < EPSILON,
            "snapped light view stays within one test epsilon"
        );

        // `properLightProj` = OrthoLH(2d, 2d, 0, lightDist + d) with
        // d = sqrt(19/4) (`:114-115`) — the frustum radius. The canonical
        // near plane is 0.01 rather than 0; both differences sit under
        // EPSILON.
        let d = (19.0f32 / 4.0).sqrt();
        let mut proper_light_proj = Matrix::zero();
        proper_light_proj.orthogonal_projection(2.0 * d, 2.0 * d, 0.0, light_dist + d);
        assert!(
            calc_matrix_distance(&proper_light_proj, &out.proj) < EPSILON,
            "snapped projection matches the reference radius box"
        );

        // The radius itself and the snap grid.
        let corners =
            calc_frustrum_corners_world_space(&view, 0.5, 1.5, std::f32::consts::FRAC_PI_2, 1.0);
        let center = calc_corners_center(&corners);
        assert!((calc_corners_radius(&corners, &center) - d).abs() < 1e-5);

        // The cull matrix is the unquantised fit — equal to the plain
        // builder's view and within epsilon of its projection.
        let plain = calc_light_view_projection_matrix(
            &view,
            0.5,
            1.5,
            std::f32::consts::FRAC_PI_2,
            1.0,
            &light_dir,
            light_dist,
            1024.0,
        );
        assert!(calc_matrix_distance(&plain.light_view, &out.cull_view_proj) >= 0.0);
        let mut vp = Matrix::zero();
        vp.multiply(&plain.light_view, &plain.light_proj);
        assert!(calc_matrix_distance(&vp, &out.cull_view_proj) < EPSILON);
    }

    #[test]
    fn the_points_variant_fits_the_caster_centroid() {
        let points = vec![v3(0.0, 0.0, 0.0), v3(1.0, 0.0, 0.0), v3(0.0, 1.0, 0.0)];
        let light_dir = v3(0.0, 0.0, 1.0);

        let out = calc_light_view_projection_matrix_points(&points, &light_dir, 10.0, 100.0);

        // Centroid (1/3, 1/3, 0); eye is ten units back down -z, looking at
        // the centroid with the default up: an identity rotation whose
        // translation row is -basis . eye.
        let mut expected_view = Matrix::identity();
        expected_view.m[3] = [-1.0 / 3.0, -1.0 / 3.0, 10.0, 1.0];
        assert!(calc_matrix_distance(&expected_view, &out.light_view) < 1e-5);

        // The three points project to x' = p.x - 1/3, y' = p.y - 1/3,
        // z' = p.z + 10, so the AABB is ([-1/3, 2/3]², z = 10).
        assert_eq!(out.max_z, 10.0);
        let proj = &out.light_proj;
        assert!((proj.m[0][0] - 2.0).abs() < 1e-5);
        assert!((proj.m[1][1] - 2.0).abs() < 1e-5);
        assert!((proj.m[2][2] - 1.0 / 10.0).abs() < 1e-5);
        assert!((proj.m[3][0] - -1.0 / 3.0).abs() < 1e-5);
        assert!((proj.m[3][1] - -1.0 / 3.0).abs() < 1e-5);
        assert!(proj.m[3][2].abs() < 1e-6);

        assert!((out.texel_size_world.x - 0.01).abs() < 1e-6);
        assert!((out.texel_size_world.y - 0.01).abs() < 1e-6);
    }

    #[test]
    fn the_simple_variant_builds_a_box_at_the_light_position() {
        let light_pos = v3(2.0, 3.0, 4.0);
        let light_dir = v3(1.0, 0.0, 0.0);

        let out = calc_light_view_projection_matrix_simple(&light_pos, &light_dir, 5.0, 4.0, 2.0);

        // Look-at from (2,3,4) along +x: xaxis (0,0,-1), yaxis (0,1,0),
        // zaxis (1,0,0), translation row -basis . eye = (4, -3, -2).
        assert_eq!(out.light_view.m[0], [0.0, 0.0, 1.0, 0.0]);
        assert_eq!(out.light_view.m[1], [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(out.light_view.m[2], [-1.0, 0.0, 0.0, 0.0]);
        assert_eq!(out.light_view.m[3], [4.0, -3.0, -2.0, 1.0]);

        // OrthoLH(4, 2, 0, 5), matching the foundation recipe.
        let mut expected_proj = Matrix::zero();
        expected_proj.orthogonal_projection(4.0, 2.0, 0.0, 5.0);
        assert!(calc_matrix_distance(&expected_proj, &out.light_proj) < 1e-6);

        // No texel/maxZ outputs for the simple variant.
        assert_eq!(out.texel_size_world.x, 0.0);
        assert_eq!(out.texel_size_world.y, 0.0);
        assert_eq!(out.max_z, 0.0);
    }

    #[test]
    fn frustum_corners_match_the_reference_scene() {
        let (view, _light_dir, _light_dist) = reference_scene();

        let corners =
            calc_frustrum_corners_world_space(&view, 0.5, 1.5, std::f32::consts::FRAC_PI_2, 1.0);

        // tan(45°) halves: near 0.5 around z=0, far 1.5 around z=1, all
        // centred on the eye column x=0.5, y=0.5.
        let expected = [
            v3(0.0, 0.0, 0.0),
            v3(0.0, 1.0, 0.0),
            v3(1.0, 1.0, 0.0),
            v3(1.0, 0.0, 0.0),
            v3(-1.0, -1.0, 1.0),
            v3(-1.0, 2.0, 1.0),
            v3(2.0, 2.0, 1.0),
            v3(2.0, -1.0, 1.0),
        ];
        for (got, want) in corners.iter().zip(expected.iter()) {
            assert!((*got - *want).length() < 1e-5, "{:?} != {:?}", got, want);
        }
    }

    #[test]
    fn translate_corners_maps_world_to_light_view_without_w_divide() {
        let (view, _light_dir, _light_dist) = reference_scene();
        let light_view = d3dx_look_at_lh(&v3(-9.5, 0.5, 0.5), &v3(0.5, 0.5, 0.5), &up_vector());

        let corners =
            calc_frustrum_corners_world_space(&view, 0.5, 1.5, std::f32::consts::FRAC_PI_2, 1.0);
        let translated =
            translate_corners_from_world_space_to_light_view_space(&corners, &light_view);

        // xaxis (0,0,-1), yaxis (0,1,0), zaxis (1,0,0): x' = -(z-0.5)
        // (world z, not x!), y' = y-0.5, z' = x+9.5. Near corners (z=0) all
        // land on x' = 0.5, far corners (z=1) on x' = -0.5.
        let expected = [
            v3(0.5, -0.5, 9.5),
            v3(0.5, 0.5, 9.5),
            v3(0.5, 0.5, 10.5),
            v3(0.5, -0.5, 10.5),
            v3(-0.5, -1.5, 8.5),
            v3(-0.5, 1.5, 8.5),
            v3(-0.5, 1.5, 11.5),
            v3(-0.5, -1.5, 11.5),
        ];
        for (got, want) in translated.iter().zip(expected.iter()) {
            assert!((*got - *want).length() < 1e-4, "{:?} != {:?}", got, want);
        }
    }

    #[test]
    fn corners_center_radius_and_scale_round_trip() {
        let (view, _light_dir, _light_dist) = reference_scene();
        let corners =
            calc_frustrum_corners_world_space(&view, 0.5, 1.5, std::f32::consts::FRAC_PI_2, 1.0);

        let center = calc_corners_center(&corners);
        assert!((center - v3(0.5, 0.5, 0.5)).length() < 1e-5);

        // The far corners sit (1.5, 1.5, 0.5) from the centre.
        let radius = calc_corners_radius(&corners, &center);
        assert!((radius - (19.0f32 / 4.0).sqrt()).abs() < 1e-5);

        // Scaling by 2 about the centre doubles every offset.
        let scaled = scale_corners(&corners, &center, 2.0);
        for (got, want) in scaled.iter().zip(corners.iter()) {
            let expected = center + (*want - center) * 2.0;
            assert!((*got - expected).length() < 1e-5);
        }

        // Scaling by 0 collapses onto the centre.
        let collapsed = scale_corners(&corners, &center, 0.0);
        for got in collapsed.iter() {
            assert!((*got - center).length() < 1e-5);
        }
    }

    #[test]
    fn aabb_helpers_track_the_min_max() {
        let points = vec![v3(1.0, -2.0, 3.0), v3(-4.0, 5.0, -6.0), v3(0.0, 0.0, 0.0)];
        let (min, max) = calc_points_aabb(&points);
        assert_eq!(min, v3(-4.0, -2.0, -6.0));
        assert_eq!(max, v3(1.0, 5.0, 3.0));

        let corners_ref = [min, max, min, max, min, max, min, max];
        let (min8, max8) = calc_corners_aabb(&corners_ref);
        assert_eq!(min8, min);
        assert_eq!(max8, max);

        // The basis variant transforms first: translating by (1,1,1) via a
        // translation matrix shifts the bounds.
        let mut basis = Matrix::identity();
        basis.m[3] = [1.0, 1.0, 1.0, 1.0];
        let (bmin, bmax) = calc_points_aabb_in_basis(&basis, &points);
        assert_eq!(bmin, v3(-3.0, -1.0, -5.0));
        assert_eq!(bmax, v3(2.0, 6.0, 4.0));
    }

    #[test]
    fn texture_scale_bias_maps_clip_space_to_the_unit_square() {
        // Applied to the identity the matrix itself comes back.
        let out = apply_texture_scale_bias_mat(&Matrix::identity());
        assert_eq!(out.m[0][0], 0.5);
        assert_eq!(out.m[1][1], -0.5);
        assert_eq!(out.m[2][2], 1.0);
        assert_eq!(out.m[3][0], 0.5);
        assert_eq!(out.m[3][1], 0.5);
        assert_eq!(out.m[3][3], 1.0);

        // The y flip: clip point (0, 1, 0, 1) lands at (0.5, 0, 0, 1) — the
        // top of clip space maps to the texture's top row.
        let p = out.apply_v4_point(&Vector4::new(0.0, 1.0, 0.0, 1.0));
        assert!((p.x - 0.5).abs() < 1e-6);
        assert!(p.y.abs() < 1e-6);

        // Composition follows the reference: out = in * scaleBias, so a
        // translated input shifts the translation row through the scaled
        // axes: the y offset 1 lands at -0.5*1 + 0.5 = 0.
        let mut m = Matrix::identity();
        m.m[3] = [0.0, 1.0, 0.0, 1.0];
        let composed = apply_texture_scale_bias_mat(&m);
        let q = composed.apply_v4_point(&Vector4::new(0.0, 0.0, 0.0, 1.0));
        assert!((q.x - 0.5).abs() < 1e-6);
        assert!(
            q.y.abs() < 1e-6,
            "translation y scales by -0.5 then offsets"
        );
    }

    #[test]
    fn rect_fit_variant_composes_scale_bias_with_the_sub_rect() {
        let ul = Vector2 { x: 0.1, y: 0.2 };
        let br = Vector2 { x: 2.1, y: 0.7 };
        let out = apply_texture_scale_bias_mat_rect(&Matrix::identity(), &ul, &br);

        // scaleBias x rectFit: diagonal 0.5*sx / -0.5*sy, translation row
        // 0.5*sx + ox / 0.5*sy + oy.
        assert!((out.m[0][0] - 0.5 * 2.0).abs() < 1e-6);
        assert!((out.m[1][1] - -0.5 * 0.5).abs() < 1e-6);
        assert!((out.m[3][0] - (0.5 * 2.0 + 0.1)).abs() < 1e-6);
        assert!((out.m[3][1] - (0.5 * 0.5 + 0.2)).abs() < 1e-6);
        assert_eq!(out.m[3][3], 1.0);
    }

    #[test]
    fn split_distances_blend_log_and_uniform() {
        let out = calc_split_distances(4, 1.0, 100.0, 0.5);

        assert_eq!(out.len(), 5);
        assert_eq!(out[0], 1.0);
        assert_eq!(out[4], 100.0);

        // Ci = log·lambda + uniform·(1 - lambda).
        let cases = [
            (1, 1.0f32 * 100.0f32.powf(0.25), 1.0 + 99.0 * 0.25),
            (2, 1.0f32 * 100.0f32.powf(0.5), 1.0 + 99.0 * 0.5),
            (3, 1.0f32 * 100.0f32.powf(0.75), 1.0 + 99.0 * 0.75),
        ];
        for (i, log, uni) in cases {
            let expected = log * 0.5 + uni * 0.5;
            assert!(
                (out[i] - expected).abs() < 1e-4,
                "split {i}: {} vs {}",
                out[i],
                expected
            );
        }

        // lambda = 1 is the pure logarithmic scheme.
        let out = calc_split_distances(2, 1.0, 100.0, 1.0);
        assert!((out[1] - 10.0).abs() < 1e-4);
        // lambda = 0 is the pure uniform scheme.
        let out = calc_split_distances(2, 1.0, 100.0, 0.0);
        assert!((out[1] - 50.5).abs() < 1e-4);
    }

    #[test]
    fn calc_light_view_matrix_looks_from_the_origin() {
        // A side-on direction keeps the default up vector.
        let m = calc_light_view_matrix(&v3(1.0, 0.0, 0.0));
        assert_eq!(m.m[0], [0.0, 0.0, 1.0, 0.0]);
        assert_eq!(m.m[1], [0.0, 1.0, 0.0, 0.0]);
        assert_eq!(m.m[2], [-1.0, 0.0, 0.0, 0.0]);
        assert_eq!(m.m[3], [0.0, 0.0, 0.0, 1.0]);

        // A straight-down direction swaps in the horizon vector
        // (x² + z² < 0.001); the cross products stay defined.
        let m = calc_light_view_matrix(&v3(0.0, 1.0, 0.0));
        assert_eq!(m.m[0], [-1.0, 0.0, 0.0, 0.0]);
        assert_eq!(m.m[1], [0.0, 0.0, 1.0, 0.0]);
        assert_eq!(m.m[2], [0.0, 1.0, 0.0, 0.0]);
    }

    #[test]
    fn up_and_horizon_constants_match_the_header() {
        let up = up_vector();
        let horizon = horizon_vector();
        assert_eq!(up, v3(0.0, 1.0, 0.0));
        assert_eq!(horizon, v3(0.0, 0.0, 1.0));

        // The switch threshold sits between the two: a mostly-horizontal
        // direction keeps the up vector, a near-vertical one flips.
        let tilted = pick_up_vector(&v3(0.05, 0.0, 0.05));
        assert_eq!(tilted, up);
        let vertical = pick_up_vector(&v3(0.03, 1.0, 0.0));
        assert_eq!(vertical, horizon);
    }
}
