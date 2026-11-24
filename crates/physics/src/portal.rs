//! BW-10-03-07: portal pass-through and the sweep coordinate transform.
//!
//! Faces ported from the reference tree:
//! - `lib/chunk/chunk_boundary.cpp:491-586` — the `Portal` plane
//!   constructor (axes, origin, centre, winding check, chunk-name
//!   dispatch) and `:895-909` — `setCollisionState`, the open/closed
//!   switch.
//! - `lib/chunk/portal_chunk_item.cpp:44-175` — `PortalObstacle`: the
//!   portal polygon as collision triangles (10cm bb, 5cm fan) with the
//!   permissive gate in both collide overloads.
//! - `lib/physics2/sweep_helpers.hpp` — `collideIntoObject`: world →
//!   obstacle-local coordinates, biggest axis, bb clip with bloat, the
//!   sTravel/eTravel remap and the window rejects.
//! - `lib/physics2/sweep_shape.hpp` — `SweepParams` and the two
//!   `SweepShape` instantiations (ray point, swept triangle).
//! - `lib/physics2/collision_callback.hpp` — `CollisionCallback`'s
//!   visit protocol (say bits, deprecated water fallback) and
//!   `CollisionState`.
//!
//! Not modeled here (registered boundaries): the chunk domain around
//! portals — `ChunkSpace` grid traversal, portal partner matching,
//! `Chunk`/`ChunkItem` identity and `SceneObject` (the obstacle keeps
//! only its transform pair and the owning portal is passed in to the
//! collide calls, since the reference reads the live portal state
//! through the item pointer).

use std::cell::RefCell;

use foundation::math::{BoundBox, Matrix, PlaneEq, Vector2, Vector3};

use crate::world_triangle::{TRIANGLE_WATER, WorldTriangle};

/// `COLLIDE_STOP` — the callback is done (`collision_callback.hpp:26`).
pub const COLLIDE_STOP: i32 = 0;
/// `COLLIDE_BEFORE` — interested in nearer collisions.
pub const COLLIDE_BEFORE: i32 = 1;
/// `COLLIDE_AFTER` — interested in further collisions.
pub const COLLIDE_AFTER: i32 = 2;
/// `COLLIDE_NOT_IMPLEMENTED` — `doVisit` was not overridden.
pub const COLLIDE_NOT_IMPLEMENTED: i32 = 4;
/// `COLLIDE_ALL` — interested in collisions before and after.
pub const COLLIDE_ALL: i32 = COLLIDE_BEFORE | COLLIDE_AFTER;

/// What the portal's `chunkName` binds (`chunk_boundary.cpp:524-545`):
/// the empty name binds nothing, the four magic names are the sentinel
/// pointers (HEAVEN/EARTH/INVASIVE/EXTERN), anything else names a new
/// `Chunk` — chunk domain, kept here as the name only.
#[derive(Debug, Clone, PartialEq)]
pub enum PortalTarget {
    None,
    Heaven,
    Earth,
    Invasive,
    Extern,
    Chunk(String),
}

/// `ChunkBoundary::Portal` — the collision-relevant face (`Portal`
/// constructor, `chunk_boundary.cpp:491-586`). The chunk-space bookkeeping
/// (traverse marks, boundary registration) stays behind.
#[derive(Debug, Clone)]
pub struct Portal {
    /// 2D points in the portal's (u, v) frame.
    pub points: Vec<Vector2>,
    pub u_axis: Vector3,
    pub v_axis: Vector3,
    pub origin: Vector3,
    pub plane: PlaneEq,
    /// Portal centre in chunk-local and world coordinates
    /// (`chunk_boundary.cpp:569-571`; equal because the ctor runs before
    /// the chunk transform lands).
    pub lcentre: Vector3,
    pub centre: Vector3,
    /// Open portals let everything through (the obstacle's gate reads
    /// this live).
    pub permissive: bool,
    pub collision_flags: u16,
    /// A closed portal has tossed its `PortalChunkItem`; opening does
    /// **not** remove the obstacle — `permissive` gates it instead
    /// (`setCollisionState`, `chunk_boundary.cpp:895-909`).
    pub has_chunk_item: bool,
    pub target: PortalTarget,
}

impl Portal {
    /// The fast `Portal` constructor (`chunk_boundary.cpp:491-586`).
    ///
    /// The reference reads `points[0..2]` unguarded for the winding check;
    /// the port asserts the minimum like a `MF_ASSERT` site.
    pub fn from_plane(
        plane: PlaneEq,
        u_axis_in: Vector3,
        points_in: &[Vector3],
        chunk_name: &str,
    ) -> Self {
        let target = match chunk_name {
            "" => PortalTarget::None,
            "heaven" => PortalTarget::Heaven,
            "earth" => PortalTarget::Earth,
            "invasive" => PortalTarget::Invasive,
            "extern" => PortalTarget::Extern,
            other => PortalTarget::Chunk(other.to_string()),
        };

        assert!(
            points_in.len() >= 3,
            "reference reads points[0..2] unguarded (MF_ASSERT site)"
        );

        // read in the polygon points (chunk_boundary.cpp:537-546).
        let mut avg = Vector2::ZERO;
        let mut points = Vec::with_capacity(points_in.len());
        for p in points_in {
            let next = Vector2::new(p.x, p.y);
            avg = avg + next;
            points.push(next);
        }
        let avg = avg / points.len() as f32;

        // read in the axes (chunk_boundary.cpp:548-551): v is the plane
        // normal cross u; the origin sits at the plane's closest point to
        // the chunk-local origin.
        let u_axis = u_axis_in;
        let v_axis = Vector3::ZERO.cross_product(plane.normal(), &u_axis);
        let origin = (*plane.normal() * plane.d()) / plane.normal().length_squared();

        // figure out the centre of our portal (chunk_boundary.cpp:569-571).
        let lcentre = u_axis * avg.x + v_axis * avg.y + origin;
        let centre = lcentre;

        // winding check (chunk_boundary.cpp:573-585): the plane through the
        // first three 3D points (normalised by default, `planeeq.ipp:97-105`)
        // must face the same way as the portal plane or the polygon is
        // reversed.
        let p3 = |i: usize| u_axis * points[i].x + v_axis * points[i].y + origin;
        let mut test_plane = PlaneEq::default();
        test_plane.init(&p3(0), &p3(1), &p3(2));

        let mut n1 = *plane.normal();
        let mut n2 = *test_plane.normal();
        n1.normalise();
        n2.normalise();
        // should be 2 if equal
        if (n1 + n2).length() < 1.0 {
            points.reverse();
        }

        Self {
            points,
            u_axis,
            v_axis,
            origin,
            plane,
            lcentre,
            centre,
            permissive: true,
            collision_flags: 0,
            has_chunk_item: false,
            target,
        }
    }

    /// `Portal::setCollisionState` (`chunk_boundary.cpp:895-909`): the
    /// open/closed switch. Closing creates the chunk item (once); opening
    /// only flips `permissive` — the obstacle stays and early-outs. The
    /// `Chunk *` argument of the reference exists for `addStaticItem`
    /// (chunk domain); the port tracks the flag.
    pub fn set_collision_state(&mut self, is_open: bool, collision_flags: u16) {
        if !self.has_chunk_item && !is_open {
            self.has_chunk_item = true;
        }

        self.permissive = is_open;
        self.collision_flags = collision_flags;
    }
}

/// `CollisionObstacle` (`physics2/collision_obstacle.hpp`): the transform
/// pair handed to the callback. The reference's `flags_`/`SceneObject`
/// payload is not modeled (registered).
#[derive(Debug, Clone, Copy)]
pub struct CollisionObstacle {
    pub transform: Matrix,
    pub transform_inverse: Matrix,
}

impl CollisionObstacle {
    pub fn new(transform: &Matrix, transform_inverse: &Matrix) -> Self {
        Self {
            transform: *transform,
            transform_inverse: *transform_inverse,
        }
    }
}

/// `CollisionCallback` (`physics2/collision_callback.hpp:21-110`): the
/// say-bit protocol. Override `do_visit` (the modern hook) and/or
/// `visit_deprecated` (the legacy `operator()`); `visit` runs the
/// reference protocol and is not meant to be overridden.
pub trait CollisionCallback {
    /// `doVisit` — default: not implemented, which triggers the
    /// deprecated fallback.
    fn do_visit(
        &mut self,
        _obstacle: &CollisionObstacle,
        _triangle: &WorldTriangle,
        _dist: f32,
    ) -> i32 {
        COLLIDE_NOT_IMPLEMENTED
    }

    /// `operator()` — the deprecated hook. Default: stop.
    fn visit_deprecated(
        &mut self,
        _obstacle: &CollisionObstacle,
        _triangle: &WorldTriangle,
        _dist: f32,
    ) -> i32 {
        COLLIDE_STOP
    }

    /// `visit` (`collision_callback.hpp:43-63`, MF_SERVER path): run
    /// `doVisit`; when it reports not-implemented, water triangles answer
    /// `COLLIDE_ALL` and everything else falls to the deprecated hook.
    fn visit(&mut self, obstacle: &CollisionObstacle, triangle: &WorldTriangle, dist: f32) -> i32 {
        let result = self.do_visit(obstacle, triangle, dist);
        if result == COLLIDE_NOT_IMPLEMENTED {
            if triangle.flags() & TRIANGLE_WATER != 0 {
                COLLIDE_ALL
            } else {
                self.visit_deprecated(obstacle, triangle, dist)
            }
        } else {
            result
        }
    }
}

/// `CollisionState` (`physics2/collision_callback.hpp:144-166`): the
/// running sweep state shared by the broad phase, the obstacle and the
/// callback.
pub struct CollisionState<'a> {
    /// user-supplied collision callback object
    pub cc: &'a mut dyn CollisionCallback,
    /// only interested in nearer than `dist`
    pub only_less: bool,
    /// only interested in further than `dist`
    pub only_more: bool,
    /// dist along line at source
    pub s_travel: f32,
    /// dist along line at extent
    pub e_travel: f32,
    /// last value passed to the callback
    pub dist: f32,
}

impl<'a> CollisionState<'a> {
    pub fn new(cc: &'a mut dyn CollisionCallback) -> Self {
        Self {
            cc,
            only_less: false,
            only_more: false,
            s_travel: 0.0,
            e_travel: 0.0,
            dist: -1.0,
        }
    }
}

/// A shape swept through the space (`sweep_shape.hpp`): either a single
/// point (a ray, `SweepShape<Vector3>`) or a triangle (a prism,
/// `SweepShape<WorldTriangle>`).
#[derive(Debug, Clone, Copy)]
pub enum SweepShape {
    Ray(Vector3),
    Prism(WorldTriangle),
}

/// The transformed shape `SweepShape::transform` produces — the argument
/// type selects which `collide` overload runs.
#[derive(Debug, Clone, Copy)]
pub enum SweptShape {
    Point(Vector3),
    Triangle(WorldTriangle),
}

impl SweepShape {
    /// `leader()` — the point the sweep direction is measured from
    /// (`sweep_shape.hpp`: `pt_` / `wt_.v0()`).
    pub fn leader(&self) -> Vector3 {
        match *self {
            SweepShape::Ray(pt) => pt,
            SweepShape::Prism(wt) => wt.v0(),
        }
    }

    /// `boundingBox` — the shape's own bounds before the sweep.
    pub fn bounding_box(&self) -> BoundBox {
        match *self {
            SweepShape::Ray(pt) => BoundBox::new(pt, pt),
            SweepShape::Prism(wt) => {
                let mut bb = BoundBox::new(wt.v0(), wt.v0());
                bb.add_bounds(&wt.v1());
                bb.add_bounds(&wt.v2());
                bb
            }
        }
    }

    /// `transformRangeToRadius` — how much the obstacle's inverse
    /// transform blooms the shape's range (`sweep_shape.hpp`): a ray adds
    /// nothing; a prism's range rides the inverse transform and halves.
    pub fn transform_range_to_radius(&self, tr_inv: &Matrix, shape_range: Vector3) -> f32 {
        match *self {
            SweepShape::Ray(_) => 0.0,
            SweepShape::Prism(_) => {
                let clip_range = tr_inv.apply_vector(&shape_range);
                clip_range.length() * 0.5
            }
        }
    }

    /// `transform` — ride the shape along the (normalised) direction
    /// between the clipped start/end parameters: a ray's shape is the
    /// clipped bb line itself; a prism's triangle rides its v0 by
    /// `sdist` and its end sits `edist` past v0.
    #[allow(clippy::too_many_arguments)]
    pub fn transform(
        &self,
        tr: &Matrix,
        sdist: f32,
        edist: f32,
        dir: Vector3,
        bb_center_at_s: Vector3,
        bb_center_at_e: Vector3,
    ) -> (SweptShape, Vector3) {
        match *self {
            SweepShape::Ray(_) => (SweptShape::Point(bb_center_at_s), bb_center_at_e),
            SweepShape::Prism(wt) => {
                let off = dir * sdist;
                let shape = WorldTriangle::new(
                    tr.apply_point(&(wt.v0() + off)),
                    tr.apply_point(&(wt.v1() + off)),
                    tr.apply_point(&(wt.v2() + off)),
                    0,
                );
                let end = tr.apply_point(&(wt.v0() + dir * edist));
                (SweptShape::Triangle(shape), end)
            }
        }
    }
}

/// `SweepParams` (`sweep_shape.hpp`): the derived sweep geometry — shape
/// bounds and range, the box/corner source and extent pair, and the
/// normalised direction with its full length.
#[derive(Debug, Clone, Copy)]
pub struct SweepParams {
    shape_box: BoundBox,
    shape_range: Vector3,
    shape_rad: f32,
    bsource: Vector3,
    bextent: Vector3,
    csource: Vector3,
    cextent: Vector3,
    dir: Vector3,
    full_dist: f32,
}

impl SweepParams {
    /// `SweepParams::SweepParams` (`sweep_shape.hpp`, template ctor).
    pub fn new(start: &SweepShape, end: Vector3) -> Self {
        // find the min and max
        let mut shape_box = start.bounding_box();

        let shape_range = shape_box.max - shape_box.min;
        let shape_rad = shape_range.length() * 0.5;
        let dir = end - start.leader();

        // find our real source and extent
        let bsource = shape_box.min;
        let bextent = bsource + dir;

        // get the source point moved to the centre of the bb
        // (for hulltree rounded cylinder collisions)
        let csource = bsource + shape_range * 0.5;
        let cextent = csource + dir;

        let dest_min = shape_box.min + dir;
        let dest_max = shape_box.max + dir;
        shape_box.add_bounds(&dest_min);
        shape_box.add_bounds(&dest_max);

        let full_dist = dir.length();
        let dir = dir / full_dist;

        Self {
            shape_box,
            shape_range,
            shape_rad,
            bsource,
            bextent,
            csource,
            cextent,
            dir,
            full_dist,
        }
    }

    pub fn shape_box(&self) -> &BoundBox {
        &self.shape_box
    }

    pub fn shape_range(&self) -> Vector3 {
        self.shape_range
    }

    pub fn shape_radius(&self) -> f32 {
        self.shape_rad
    }

    pub fn bsource(&self) -> Vector3 {
        self.bsource
    }

    pub fn bextent(&self) -> Vector3 {
        self.bextent
    }

    pub fn csource(&self) -> Vector3 {
        self.csource
    }

    pub fn cextent(&self) -> Vector3 {
        self.cextent
    }

    pub fn direction(&self) -> Vector3 {
        self.dir
    }

    pub fn full_distance(&self) -> f32 {
        self.full_dist
    }
}

/// `SweepHelpers::biggestAxisOfDifference` (`sweep_helpers.hpp`): the
/// axis with the largest absolute difference. Strictly greater both
/// times, and the z comparison never updates the running difference —
/// the reference quirk kept verbatim.
pub fn biggest_axis_of_difference(a: &Vector3, b: &Vector3) -> usize {
    let mut bax = 0;
    let mut diff1 = (a.x - b.x).abs();
    let diff2 = (a.y - b.y).abs();
    if diff2 > diff1 {
        diff1 = diff2;
        bax = 1;
    }

    let diff2 = (a.z - b.z).abs();
    if diff2 > diff1 {
        bax = 2;
    }

    bax
}

/// `CollisionInterface` (`physics2/collision_interface.hpp`): what an
/// obstacle exposes to the sweep broad phase. The port threads the
/// owning `Portal` through (the reference reaches it via the obstacle's
/// `ChunkItem` pointer); `permissive` and the collision flags must be
/// read live.
pub trait CollisionInterface {
    /// `collide(source, extent, state)` — the extruded point overload.
    fn collide_ray(
        &self,
        portal: &Portal,
        source: Vector3,
        extent: Vector3,
        state: &mut CollisionState,
    ) -> bool;

    /// `collide(source, extent, state)` — the extruded triangle overload.
    fn collide_prism(
        &self,
        portal: &Portal,
        source: &WorldTriangle,
        extent: Vector3,
        state: &mut CollisionState,
    ) -> bool;
}

/// `PortalObstacle` (`chunk/portal_chunk_item.cpp:44-175`): the collision
/// item for a portal. The bb is built from the portal points extended
/// 10cm into the chunk; the triangles are a fan extended 5cm. Both
/// collide overloads start by checking `permissive` — an open portal
/// lets everything through (the obstacle is deliberately kept after
/// `setCollisionState` opens the portal).
#[derive(Debug)]
pub struct PortalObstacle {
    pub transform: Matrix,
    pub transform_inverse: Matrix,
    pub bb: BoundBox,
    /// `mutable BW::vector<WorldTriangle> ltris_` — the visit rewrites
    /// each hit triangle's flags with the portal's collision byte.
    ltris: RefCell<Vec<WorldTriangle>>,
}

impl PortalObstacle {
    /// `PortalObstacle::PortalObstacle` (`portal_chunk_item.cpp:82-104`).
    /// `ChunkObstacle` stores the chunk transform and inverts it
    /// (`chunk_obstacle.cpp:43-54`); the bb grows from the portal points.
    pub fn new(chunk_transform: &Matrix, portal: &Portal) -> Self {
        let transform = *chunk_transform;
        let mut transform_inverse = transform;
        transform_inverse.invert();

        // extend 10cm into the chunk (the normal is always normalised)
        let pt_extra = *portal.plane.normal() * 0.10;

        // build up the bb from the portal points
        let mut bb = BoundBox::default();
        for (i, p) in portal.points.iter().enumerate() {
            let pt = portal.u_axis * p.x + portal.v_axis * p.y + portal.origin;

            if i == 0 {
                bb = BoundBox::new(pt, pt);
            } else {
                bb.add_bounds(&pt);
            }
            bb.add_bounds(&(pt + pt_extra));
        }

        let ltris = Self::build_triangles(portal);
        Self {
            transform,
            transform_inverse,
            bb,
            ltris: RefCell::new(ltris),
        }
    }

    /// `buildTriangles` (`portal_chunk_item.cpp:112-152`): a triangle fan
    /// over the portal points, extended 5cm into the chunk. Fewer than
    /// three points clears the list.
    fn build_triangles(portal: &Portal) -> Vec<WorldTriangle> {
        let mut ltris = Vec::new();

        if portal.points.len() < 3 {
            return ltris;
        }

        // extend 5cm into the chunk
        let pt_ex_ori = portal.origin + *portal.plane.normal() * 0.05;

        // build the wt's from the points
        let p3 = |p: &Vector2| portal.u_axis * p.x + portal.v_axis * p.y + pt_ex_ori;
        let pto = p3(&portal.points[0]);
        let mut ptb = p3(&portal.points[1]);

        for p in &portal.points[2..] {
            // shuffle and find the next pt
            let pta = ptb;
            ptb = p3(p);

            // make a triangle then
            ltris.push(WorldTriangle::new(pto, pta, ptb, 0));
        }
        ltris
    }

    fn visit_hit(&self, portal: &Portal, i: usize, state: &mut CollisionState, dist: f32) -> i32 {
        state.dist = dist;

        // call the callback function
        let tri = {
            let mut ltris = self.ltris.borrow_mut();
            ltris[i].set_flags(portal.collision_flags & 0xff);
            ltris[i]
        };

        let ob = CollisionObstacle::new(&self.transform, &self.transform_inverse);
        state.cc.visit(&ob, &tri, state.dist)
    }

    /// The shared say-bit tail of both collides (`portal_chunk_item.cpp:
    /// 190-200` / `223-233`).
    fn apply_say(state: &mut CollisionState, say: i32) -> bool {
        // see if any other collisions are wanted
        if say == COLLIDE_STOP {
            return true;
        }

        // some are wanted ... see if it's only one side
        state.only_less = (say & 2) == 0;
        state.only_more = (say & 1) == 0;
        false
    }
}

impl CollisionInterface for PortalObstacle {
    /// Collision test with an extruded point (`portal_chunk_item.cpp:
    /// 158-206`).
    fn collide_ray(
        &self,
        portal: &Portal,
        source: Vector3,
        extent: Vector3,
        state: &mut CollisionState,
    ) -> bool {
        // see if we let anyone through
        if portal.permissive {
            return false;
        }

        // ok, see if they collide then
        // (chances are very high if they're in the bb!)
        let tranl = extent - source;
        let count = self.ltris.borrow().len();
        for i in 0..count {
            // see if it intersects
            let mut rd = 1.0f32;
            if !self.ltris.borrow()[i].intersects(source, tranl, &mut rd) {
                continue;
            }

            // see how far we really travelled (handles scaling, etc.)
            let ndist = state.s_travel + (state.e_travel - state.s_travel) * rd;

            if state.only_less && ndist > state.dist {
                continue;
            }
            if state.only_more && ndist < state.dist {
                continue;
            }

            let say = self.visit_hit(portal, i, state, ndist);
            if Self::apply_say(state, say) {
                return true;
            }
        }

        false
    }

    /// Collision test with an extruded triangle (`portal_chunk_item.cpp:
    /// 210-252`). A prism reports its start parameter as the distance.
    fn collide_prism(
        &self,
        portal: &Portal,
        source: &WorldTriangle,
        extent: Vector3,
        state: &mut CollisionState,
    ) -> bool {
        // see if we let anyone through
        if portal.permissive {
            return false;
        }

        // ok, see if they collide then
        // (chances are very high if they're in the bb!)
        let tranl = extent - source.v0();
        let count = self.ltris.borrow().len();
        for i in 0..count {
            // see if it intersects
            if !self.ltris.borrow()[i].intersects_swept(source, tranl) {
                continue;
            }

            // see how far we really travelled
            let ndist = state.s_travel;

            if state.only_less && ndist > state.dist {
                continue;
            }
            if state.only_more && ndist < state.dist {
                continue;
            }

            let say = self.visit_hit(portal, i, state, ndist);
            if Self::apply_say(state, say) {
                return true;
            }
        }

        false
    }
}

/// `SweepHelpers::collideIntoObject` (`sweep_helpers.hpp`): the broad
/// phase's per-obstacle decision. The sweep's centre line is taken into
/// obstacle-local coordinates, the biggest axis of the difference picks
/// the travel axis, the bb clip (with the shape's bloated radius)
/// rejects or narrows the window, and the travel parameters are remapped
/// to the sweep's full distance before the narrowed shape goes into the
/// obstacle's collide.
#[allow(clippy::too_many_arguments)]
pub fn collide_into_object(
    start: &SweepShape,
    sp: &SweepParams,
    collision_funcs: &dyn CollisionInterface,
    portal: &Portal,
    obstacle_local_bb: &BoundBox,
    obstacle_inv_transform: &Matrix,
    cs: &mut CollisionState,
) -> bool {
    let s_tr = obstacle_inv_transform.apply_point(&sp.csource());
    let e_tr = obstacle_inv_transform.apply_point(&sp.cextent());

    let bax = biggest_axis_of_difference(&e_tr, &s_tr);

    let s_trba = s_tr.get(bax);
    let d_trba = e_tr.get(bax) - s_trba;

    // Clip the line to the bounding box ('tho it should always
    //  be inside since we found it through the hull tree)
    let bloat = start.transform_range_to_radius(obstacle_inv_transform, sp.shape_range()) + 0.01;

    let mut s_tr_clipped = s_tr;
    let mut e_tr_clipped = e_tr;
    if !obstacle_local_bb.clip(&mut s_tr_clipped, &mut e_tr_clipped, bloat) {
        return false;
    }

    // set traveled and traveled to be the start and end distances
    //  along the line (not their original use, but it fits)
    cs.s_travel = (s_tr_clipped.get(bax) - s_trba) / d_trba * sp.full_distance();
    cs.e_travel = (e_tr_clipped.get(bax) - s_trba) / d_trba * sp.full_distance();

    // see if we can reject this bb outright
    if cs.only_less && cs.s_travel > cs.dist {
        return false;
    }

    if cs.only_more && cs.e_travel < cs.dist {
        return false;
    }

    // ok, let's search in it then
    let (shape, shape_end) = start.transform(
        obstacle_inv_transform,
        cs.s_travel,
        cs.e_travel,
        sp.direction(),
        s_tr_clipped,
        e_tr_clipped,
    );

    match shape {
        SweptShape::Point(p) => collision_funcs.collide_ray(portal, p, shape_end, cs),
        SweptShape::Triangle(t) => collision_funcs.collide_prism(portal, &t, shape_end, cs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_triangle::TRIANGLE_WATER;

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    /// The rectangular portal of the truth fixture: chunk-local x = 0
    /// plane, u along +z, points (−2,−1),(2,−1),(2,3),(−2,3).
    fn rect_portal() -> Portal {
        let pts = vec![
            v(-2.0, -1.0, 0.0),
            v(2.0, -1.0, 0.0),
            v(2.0, 3.0, 0.0),
            v(-2.0, 3.0, 0.0),
        ];
        Portal::from_plane(
            PlaneEq::new(v(1.0, 0.0, 0.0), 0.0),
            v(0.0, 0.0, 1.0),
            &pts,
            "",
        )
    }

    #[test]
    fn the_plane_ctor_builds_axes_origin_and_centre() {
        let p = rect_portal();

        assert_eq!(*p.plane.normal(), v(1.0, 0.0, 0.0));
        assert_eq!(p.plane.d(), 0.0);
        assert_eq!(p.u_axis, v(0.0, 0.0, 1.0));
        // v = normal x u = (0,-1,0)
        assert_eq!(p.v_axis, v(0.0, -1.0, 0.0));
        assert_eq!(p.origin, Vector3::ZERO);
        assert_eq!(p.points.len(), 4);
        assert_eq!(p.points[0], Vector2::new(-2.0, -1.0));
        assert_eq!(p.points[3], Vector2::new(-2.0, 3.0));
        // centre = average point: (0, 1) -> (0, -1, 0) * 1 + origin
        assert_eq!(p.lcentre, v(0.0, -1.0, 0.0));
        assert_eq!(p.centre, p.lcentre);
        // ctor defaults
        assert!(p.permissive);
        assert_eq!(p.collision_flags, 0);
        assert!(!p.has_chunk_item);
        assert_eq!(p.target, PortalTarget::None);
    }

    #[test]
    fn the_plane_ctor_keeps_a_consistent_winding() {
        // The fixture winding answers a same-facing test plane: no reverse.
        let p = rect_portal();
        assert_eq!(p.points[1], Vector2::new(2.0, -1.0));
    }

    #[test]
    fn the_plane_ctor_reverses_an_opposing_winding() {
        // Hand the same rectangle wound the other way: the test plane's
        // normal opposes the portal plane, (n1+n2).length() < 1, and the
        // ctor reverses the list back into the consistent order.
        let pts = vec![
            v(-2.0, 3.0, 0.0),
            v(2.0, 3.0, 0.0),
            v(2.0, -1.0, 0.0),
            v(-2.0, -1.0, 0.0),
        ];
        let p = Portal::from_plane(
            PlaneEq::new(v(1.0, 0.0, 0.0), 0.0),
            v(0.0, 0.0, 1.0),
            &pts,
            "",
        );
        assert_eq!(p.points[0], Vector2::new(-2.0, -1.0));
        assert_eq!(p.points[1], Vector2::new(2.0, -1.0));
        assert_eq!(p.points[2], Vector2::new(2.0, 3.0));
        assert_eq!(p.points[3], Vector2::new(-2.0, 3.0));
    }

    #[test]
    fn the_ctor_maps_chunk_names_like_the_reference() {
        let pts = vec![v(-2.0, -1.0, 0.0), v(2.0, -1.0, 0.0), v(2.0, 3.0, 0.0)];
        let mk = |name: &str| {
            Portal::from_plane(
                PlaneEq::new(v(1.0, 0.0, 0.0), 0.0),
                v(0.0, 0.0, 1.0),
                &pts,
                name,
            )
            .target
        };
        assert_eq!(mk(""), PortalTarget::None);
        assert_eq!(mk("heaven"), PortalTarget::Heaven);
        assert_eq!(mk("earth"), PortalTarget::Earth);
        assert_eq!(mk("invasive"), PortalTarget::Invasive);
        assert_eq!(mk("extern"), PortalTarget::Extern);
        assert_eq!(mk("2_3"), PortalTarget::Chunk("2_3".to_string()));
    }

    #[test]
    fn set_collision_state_follows_the_reference_switch() {
        let mut p = rect_portal();
        // close: creates the chunk item once and stores the flags
        p.set_collision_state(false, 0x2103);
        assert!(p.has_chunk_item);
        assert!(!p.permissive);
        assert_eq!(p.collision_flags, 0x2103);
        // open: permissive flips, the obstacle stays (has_chunk_item kept)
        p.set_collision_state(true, 0);
        assert!(p.has_chunk_item);
        assert!(p.permissive);
        assert_eq!(p.collision_flags, 0);
        // close again: has_chunk_item already set
        p.set_collision_state(false, 0);
        assert!(p.has_chunk_item);
        assert!(!p.permissive);
    }

    #[test]
    fn the_obstacle_bb_extends_10cm_into_the_chunk() {
        let portal = rect_portal();
        let xform = Matrix::identity();
        let ob = PortalObstacle::new(&xform, &portal);
        // points map to (0, 1..-3, ±2), plus the 0.1 x-offset
        assert_eq!(ob.bb.min, v(0.0, -3.0, -2.0));
        assert_eq!(ob.bb.max, v(0.1, 1.0, 2.0));
    }

    #[test]
    fn the_obstacle_builds_a_fan_5cm_into_the_chunk() {
        let portal = rect_portal();
        let xform = Matrix::identity();
        let ob = PortalObstacle::new(&xform, &portal);
        let ltris = ob.ltris.borrow();
        // n points -> n-2 fan triangles (4 points -> 2)
        assert_eq!(ltris.len(), 2);
        // the fan apex is points[0] lifted 5cm into the chunk
        assert_eq!(ltris[0].v0(), v(0.05, 1.0, -2.0));
        assert_eq!(ltris[0].v1(), v(0.05, 1.0, 2.0));
        assert_eq!(ltris[0].v2(), v(0.05, -3.0, 2.0));
        // shuffle semantics: tri 1 walks points[2] -> points[3]
        assert_eq!(ltris[1].v0(), v(0.05, 1.0, -2.0));
        assert_eq!(ltris[1].v1(), v(0.05, -3.0, 2.0));
        assert_eq!(ltris[1].v2(), v(0.05, -3.0, -2.0));
        // the default flags word is zero
        assert_eq!(ltris[0].flags(), 0);
    }

    #[test]
    fn the_obstacle_inverts_the_chunk_transform() {
        let mut xform = Matrix::zero();
        xform.set_translate(10.0, 0.0, 5.0);
        let portal = rect_portal();
        let ob = PortalObstacle::new(&xform, &portal);
        let back = ob
            .transform_inverse
            .apply_point(&xform.apply_point(&v(1.0, 2.0, 3.0)));
        assert!((back.x - 1.0).abs() < 1e-4);
        assert!((back.y - 2.0).abs() < 1e-4);
        assert!((back.z - 3.0).abs() < 1e-4);
    }

    #[test]
    fn a_degenerate_portal_builds_no_triangles() {
        // two points: buildTriangles clears the list; the ctor itself
        // still needs three points for the winding check, so this portal
        // is built by truncating a valid one.
        let mut portal = rect_portal();
        portal.points.truncate(2);
        assert!(PortalObstacle::build_triangles(&portal).is_empty());
    }

    #[test]
    fn biggest_axis_strictly_greater_and_z_never_updates_diff() {
        assert_eq!(
            biggest_axis_of_difference(&v(0.0, 0.0, 0.0), &v(5.0, 1.0, 1.0)),
            0
        );
        assert_eq!(
            biggest_axis_of_difference(&v(0.0, 0.0, 0.0), &v(1.0, 5.0, 1.0)),
            1
        );
        assert_eq!(
            biggest_axis_of_difference(&v(0.0, 0.0, 0.0), &v(1.0, 1.0, 5.0)),
            2
        );
        // y tie keeps x (strict >)
        assert_eq!(
            biggest_axis_of_difference(&v(0.0, 0.0, 0.0), &v(3.0, 3.0, 1.0)),
            0
        );
        // z bigger than y's diff takes index 2 without touching diff1:
        // diffs x=1.9, y=1.9 tie (x stays), z=1.95 wins -> 2
        assert_eq!(
            biggest_axis_of_difference(&v(0.0, 0.0, 0.0), &v(-1.9, 1.9, -1.95)),
            2
        );
        // diffs x=1.0, y=1.9 -> diff1=1.9; z=1.9 is NOT > diff1 -> stays 1
        assert_eq!(
            biggest_axis_of_difference(&v(0.0, 0.0, 0.0), &v(-1.0, 1.9, -1.9)),
            1
        );
    }

    #[test]
    fn sweep_params_derive_the_ray_geometry() {
        let start = SweepShape::Ray(v(-5.0, 0.0, 0.0));
        let sp = SweepParams::new(&start, v(5.0, 0.0, 0.0));

        // a degenerate box: zero range and radius
        assert_eq!(sp.shape_range(), Vector3::ZERO);
        assert_eq!(sp.shape_radius(), 0.0);
        assert_eq!(sp.bsource(), v(-5.0, 0.0, 0.0));
        assert_eq!(sp.bextent(), v(5.0, 0.0, 0.0));
        // centre line == the ray itself
        assert_eq!(sp.csource(), v(-5.0, 0.0, 0.0));
        assert_eq!(sp.cextent(), v(5.0, 0.0, 0.0));
        // the box grew to cover the sweep
        assert_eq!(sp.shape_box().min, v(-5.0, 0.0, 0.0));
        assert_eq!(sp.shape_box().max, v(5.0, 0.0, 0.0));
        assert_eq!(sp.full_distance(), 10.0);
        assert_eq!(sp.direction(), v(1.0, 0.0, 0.0));
    }

    #[test]
    fn sweep_params_derive_the_prism_geometry() {
        let tri = WorldTriangle::new(v(0.0, 0.0, 0.0), v(2.0, 0.0, 0.0), v(0.0, 2.0, 0.0), 0);
        let start = SweepShape::Prism(tri);
        let sp = SweepParams::new(&start, v(0.0, 0.0, 4.0));

        assert_eq!(sp.shape_range(), v(2.0, 2.0, 0.0));
        assert_eq!(sp.shape_radius(), 2.0f32.hypot(2.0) * 0.5);
        assert_eq!(sp.csource(), v(1.0, 1.0, 0.0));
        assert_eq!(sp.cextent(), v(1.0, 1.0, 4.0));
        assert_eq!(sp.full_distance(), 4.0);
        assert_eq!(sp.direction(), v(0.0, 0.0, 1.0));
        // transformRangeToRadius rides the inverse transform
        let r = start.transform_range_to_radius(&Matrix::identity(), sp.shape_range());
        assert_eq!(r, 2.0f32.hypot(2.0) * 0.5);
        assert_eq!(
            SweepShape::Ray(v(0.0, 0.0, 0.0))
                .transform_range_to_radius(&Matrix::identity(), sp.shape_range()),
            0.0
        );
    }

    #[test]
    fn sweep_shape_transform_rides_the_sweep() {
        let tri = WorldTriangle::new(v(0.0, 0.0, 0.0), v(2.0, 0.0, 0.0), v(0.0, 2.0, 0.0), 0);
        let start = SweepShape::Prism(tri);
        let (shape, end) = start.transform(
            &Matrix::identity(),
            0.25,
            1.0,
            v(0.0, 0.0, 1.0),
            v(9.0, 9.0, 9.0),
            v(8.0, 8.0, 8.0),
        );
        match shape {
            SweptShape::Triangle(t) => {
                // every vertex rides v0's offset (0,0,0.25)
                assert_eq!(t.v0(), v(0.0, 0.0, 0.25));
                assert_eq!(t.v1(), v(2.0, 0.0, 0.25));
                assert_eq!(t.v2(), v(0.0, 2.0, 0.25));
            }
            _ => panic!("prism in, prism out"),
        }
        assert_eq!(end, v(0.0, 0.0, 1.0));

        // a ray's shape is the clipped bb line; the ride params are unused
        let ray = SweepShape::Ray(v(1.0, 2.0, 3.0));
        let (shape, end) = ray.transform(
            &Matrix::identity(),
            0.9,
            9.9,
            v(0.0, 0.0, 1.0),
            v(4.0, 5.0, 6.0),
            v(7.0, 8.0, 9.0),
        );
        match shape {
            SweptShape::Point(p) => assert_eq!(p, v(4.0, 5.0, 6.0)),
            _ => panic!("ray in, point out"),
        }
        assert_eq!(end, v(7.0, 8.0, 9.0));
        assert_eq!(ray.leader(), v(1.0, 2.0, 3.0));
        assert_eq!(SweepShape::Prism(tri).leader(), v(0.0, 0.0, 0.0));
    }

    /// A callback that records visits and answers a fixed say value.
    struct Recorder {
        say: i32,
        seen: Vec<(Vector3, WorldTriangle, f32)>,
    }

    impl Recorder {
        fn new(say: i32) -> Self {
            Self {
                say,
                seen: Vec::new(),
            }
        }
    }

    impl CollisionCallback for Recorder {
        fn do_visit(
            &mut self,
            obstacle: &CollisionObstacle,
            triangle: &WorldTriangle,
            dist: f32,
        ) -> i32 {
            self.seen
                .push((obstacle.transform.apply_to_origin(), *triangle, dist));
            self.say
        }
    }

    /// A colliding sweep against a closed identity-transform portal.
    fn hit_setup() -> (Portal, PortalObstacle) {
        let mut portal = rect_portal();
        portal.set_collision_state(false, 0x03);
        let ob = PortalObstacle::new(&Matrix::identity(), &portal);
        (portal, ob)
    }

    #[test]
    fn an_open_portal_answers_miss_without_visits() {
        let (portal, ob) = {
            let p = rect_portal(); // default permissive
            let o = PortalObstacle::new(&Matrix::identity(), &p);
            (p, o)
        };
        let (hit, visits, dist_after) = {
            let mut cb = Recorder::new(COLLIDE_STOP);
            let mut cs = CollisionState::new(&mut cb);
            let start = SweepShape::Ray(v(-5.0, -1.0, 0.0));
            let sp = SweepParams::new(&start, v(5.0, -1.0, 0.0));
            let hit = collide_into_object(
                &start,
                &sp,
                &ob,
                &portal,
                &ob.bb,
                &ob.transform_inverse,
                &mut cs,
            );
            let dist_after = cs.dist;
            (hit, cb.seen.len(), dist_after)
        };
        assert!(!hit);
        assert_eq!(visits, 0);
        // nothing touched the state
        assert_eq!(dist_after, -1.0);
    }

    #[test]
    fn a_closed_portal_answers_the_ray_hit_and_truncates_flags() {
        let (portal, ob) = hit_setup();
        let (hit, seen, dist_after) = {
            let mut cb = Recorder::new(COLLIDE_STOP);
            let mut cs = CollisionState::new(&mut cb);
            let start = SweepShape::Ray(v(-5.0, -1.0, 0.0));
            let sp = SweepParams::new(&start, v(5.0, -1.0, 0.0));
            let hit = collide_into_object(
                &start,
                &sp,
                &ob,
                &portal,
                &ob.bb,
                &ob.transform_inverse,
                &mut cs,
            );
            let dist_after = cs.dist;
            let seen = cb.seen.clone();
            (hit, seen, dist_after)
        };
        assert!(hit);
        assert_eq!(seen.len(), 1);
        let (_, tri, dist) = seen[0];
        // the collision byte is the uint8 truncation of the portal flags
        assert_eq!(tri.flags(), 0x03);
        // the hit sits at x = 0.05 (the fan plane); ndist is in full-sweep
        // units: sTravel 4.99 + (eTravel-sTravel 0.12) * rd 0.5 = 5.05
        assert!((dist - 5.05).abs() < 1e-3, "dist {dist}");
        assert_eq!(dist_after, dist);
    }

    #[test]
    fn the_prism_reports_its_start_parameter() {
        let (portal, ob) = hit_setup();
        let (seen, s_travel) = {
            let mut cb = Recorder::new(COLLIDE_STOP);
            let mut cs = CollisionState::new(&mut cb);
            let tri = WorldTriangle::new(
                v(-5.0, -1.0, 0.0),
                v(-3.0, -1.0, 0.0),
                v(-5.0, -1.0, 1.0),
                0,
            );
            let start = SweepShape::Prism(tri);
            let sp = SweepParams::new(&start, v(5.0, -1.0, 0.0));
            assert!(collide_into_object(
                &start,
                &sp,
                &ob,
                &portal,
                &ob.bb,
                &ob.transform_inverse,
                &mut cs,
            ));
            let s_travel = cs.s_travel;
            let seen = cb.seen.clone();
            (seen, s_travel)
        };
        assert_eq!(seen.len(), 1);
        let (_, _, dist) = seen[0];
        // ndist = sTravel for prisms
        assert_eq!(dist, s_travel);
    }

    #[test]
    fn the_bb_clip_rejects_probes_outside_the_box() {
        let (portal, ob) = hit_setup();
        // z = 2.5 is beyond the bb max z (2.0) + bloat 0.01
        let (visits, s_travel, e_travel) = {
            let mut cb = Recorder::new(COLLIDE_STOP);
            let mut cs = CollisionState::new(&mut cb);
            let start = SweepShape::Ray(v(-5.0, -1.0, 2.5));
            let sp = SweepParams::new(&start, v(5.0, -1.0, 2.5));
            assert!(!collide_into_object(
                &start,
                &sp,
                &ob,
                &portal,
                &ob.bb,
                &ob.transform_inverse,
                &mut cs,
            ));
            let s_travel = cs.s_travel;
            let e_travel = cs.e_travel;
            (cb.seen.len(), s_travel, e_travel)
        };
        assert_eq!(visits, 0);
        // travel untouched by the rejection
        assert_eq!(s_travel, 0.0);
        assert_eq!(e_travel, 0.0);
    }

    #[test]
    fn the_say_bits_drive_the_window_flags() {
        let (portal, ob) = hit_setup();
        let sweep = |say: i32| {
            let mut cb = Recorder::new(say);
            let mut cs = CollisionState::new(&mut cb);
            let start = SweepShape::Ray(v(-5.0, -1.0, 0.0));
            let sp = SweepParams::new(&start, v(5.0, -1.0, 0.0));
            let hit = collide_into_object(
                &start,
                &sp,
                &ob,
                &portal,
                &ob.bb,
                &ob.transform_inverse,
                &mut cs,
            );
            let (only_less, only_more) = (cs.only_less, cs.only_more);
            (hit, cb.seen.len(), only_less, only_more)
        };

        // COLLIDE_ALL (3): keeps going; both windows open.
        let (hit, visits, only_less, only_more) = sweep(COLLIDE_ALL);
        assert!(!hit);
        assert_eq!(
            visits, 2,
            "the whole two-triangle fan visits under COLLIDE_ALL"
        );
        assert!(!only_less);
        assert!(!only_more);

        // COLLIDE_BEFORE (1): onlyLess set.
        let (hit, visits, only_less, only_more) = sweep(COLLIDE_BEFORE);
        assert!(!hit);
        assert_eq!(visits, 2);
        assert!(only_less);
        assert!(!only_more);
    }

    #[test]
    fn only_less_rejects_later_hits_beyond_dist() {
        let (portal, ob) = hit_setup();
        // Stop at the first hit, then re-sweep asking only for nearer
        // collisions: the fan's later triangles are beyond dist and skip.
        let start = SweepShape::Ray(v(-5.0, -1.0, 0.0));
        let sp = SweepParams::new(&start, v(5.0, -1.0, 0.0));

        let first_dist = {
            let mut cb = Recorder::new(COLLIDE_STOP);
            let mut cs = CollisionState::new(&mut cb);
            assert!(collide_into_object(
                &start,
                &sp,
                &ob,
                &portal,
                &ob.bb,
                &ob.transform_inverse,
                &mut cs,
            ));
            cb.seen[0].2
        };

        let visits = {
            let mut cb = Recorder::new(COLLIDE_ALL);
            let mut cs = CollisionState::new(&mut cb);
            cs.only_less = true;
            cs.dist = first_dist;
            assert!(!collide_into_object(
                &start,
                &sp,
                &ob,
                &portal,
                &ob.bb,
                &ob.transform_inverse,
                &mut cs,
            ));
            cb.seen.len()
        };
        // both fan triangles sit on the same plane, so each ndist equals
        // dist — `ndist > dist` is false, so they still visit.
        assert_eq!(visits, 2);

        // A slightly nearer dist rejects them all.
        let visits = {
            let mut cb = Recorder::new(COLLIDE_ALL);
            let mut cs = CollisionState::new(&mut cb);
            cs.only_less = true;
            cs.dist = first_dist - 1e-3;
            assert!(!collide_into_object(
                &start,
                &sp,
                &ob,
                &portal,
                &ob.bb,
                &ob.transform_inverse,
                &mut cs,
            ));
            cb.seen.len()
        };
        assert_eq!(visits, 0);
    }

    #[test]
    fn only_more_rejects_hits_before_dist() {
        let (portal, ob) = hit_setup();
        let start = SweepShape::Ray(v(-5.0, -1.0, 0.0));
        let sp = SweepParams::new(&start, v(5.0, -1.0, 0.0));

        let visits = {
            let mut cb = Recorder::new(COLLIDE_ALL);
            let mut cs = CollisionState::new(&mut cb);
            cs.only_more = true;
            cs.dist = f32::MAX;
            assert!(!collide_into_object(
                &start,
                &sp,
                &ob,
                &portal,
                &ob.bb,
                &ob.transform_inverse,
                &mut cs,
            ));
            cb.seen.len()
        };
        assert_eq!(visits, 0, "ndist < dist everywhere: all skipped");
    }

    #[test]
    fn the_deprecated_water_fallback_answers_all() {
        // A default doVisit (not implemented) + water triangle ->
        // COLLIDE_ALL straight from the deprecated branch.
        let water = WorldTriangle::new(Vector3::ZERO, v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 0);
        let water = {
            let mut t = water;
            t.set_flags(TRIANGLE_WATER);
            t
        };
        struct NullCb;
        impl CollisionCallback for NullCb {}
        let mut cb = NullCb;
        let ob = CollisionObstacle::new(&Matrix::identity(), &Matrix::identity());
        assert_eq!(cb.visit(&ob, &water, 1.0), COLLIDE_ALL);
        // a dry triangle falls to operator() -> COLLIDE_STOP
        let dry = {
            let mut t = water;
            t.set_flags(0);
            t
        };
        assert_eq!(cb.visit(&ob, &dry, 1.0), COLLIDE_STOP);

        // an overridden deprecated hook receives the non-water visit
        struct DeprecatedCb;
        impl CollisionCallback for DeprecatedCb {
            fn visit_deprecated(
                &mut self,
                _obstacle: &CollisionObstacle,
                _triangle: &WorldTriangle,
                _dist: f32,
            ) -> i32 {
                COLLIDE_BEFORE
            }
        }
        let mut dcb = DeprecatedCb;
        assert_eq!(dcb.visit(&ob, &dry, 2.0), COLLIDE_BEFORE);
    }

    #[test]
    fn the_collide_state_starts_at_the_reference_defaults() {
        let mut cb = Recorder::new(COLLIDE_STOP);
        let cs = CollisionState::new(&mut cb);
        assert!(!cs.only_less);
        assert!(!cs.only_more);
        assert_eq!(cs.s_travel, 0.0);
        assert_eq!(cs.e_travel, 0.0);
        assert_eq!(cs.dist, -1.0);
    }
}
