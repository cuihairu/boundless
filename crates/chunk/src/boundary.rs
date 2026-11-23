//! BW-10-01-02 chunk boundaries and portals.
//!
//! A port of `lib/chunk/chunk_boundary.cpp` (the `ChunkBoundary` and
//! `ChunkBoundary::Portal` constructors, `inside`/`project`/`objectSpacePoint`)
//! and of `createBoundaries` (`chunk.cpp:661-851`), which synthesises the six
//! boundaries of an outside chunk from the grid. The six planes, u-axes, point
//! lists and neighbour targets below are transcribed from that function
//! verbatim; the winding check that may reverse a portal's points is ported
//! from the portal constructors (`chunk_boundary.cpp:463-474`, `584-594`).
//!
//! Server-port scope notes (registered in `docs/review/bw10-01-chunk-load.md`):
//! the `EDITOR_ENABLED` point snapping block is editor-only and skipped; the
//! mixed internal/external portal `MF_ASSERT_DEV` is a dev-only assert in the
//! reference and does not fail the parse here; `createBoundarySections`
//! (generating boundaries from a visual's triangles) belongs to the visual
//! pipeline and is not ported, so a shell visual without `<boundary>` sections
//! contributes no boundaries.

use foundation::math::{PlaneEq, Vector2, Vector3};
use resources::datasection::DataSection;

use crate::chunk::{MAX_CHUNK_HEIGHT, MIN_CHUNK_HEIGHT};
use crate::identifier::{grid_from_chunk_name, is_outside_chunk};
use crate::mapping::GeometryMapping;

/// What a portal connects to. The reference encodes the special targets as
/// small pointer values (`Portal::HEAVEN`..`EXTERN` = 1..4, `NULL` = 0) and
/// creates the target `Chunk` eagerly for real names
/// (`chunk_boundary.cpp:382-406`); here the name is kept as a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalTarget {
    /// `pChunk == NULL`: the portal is not connected to anything yet.
    Nothing,
    /// `pChunk == (Chunk*)HEAVEN`.
    Heaven,
    /// `pChunk == (Chunk*)EARTH`.
    Earth,
    /// `pChunk == (Chunk*)INVASIVE`.
    Invasive,
    /// `pChunk == (Chunk*)EXTERN`.
    Extern,
    /// A named chunk (`pChunk == new Chunk(chunkName, pMapping)`).
    Chunk(String),
}

impl PortalTarget {
    /// `Portal::hasChunk` (`chunk_boundary.hpp:261-262`): connected to a real
    /// chunk, not a special value and not empty.
    pub fn has_chunk(&self) -> bool {
        matches!(self, Self::Chunk(_))
    }

    /// `Portal::isConnectingToSpecial` (`chunk_boundary.hpp:281-285`).
    pub fn is_connecting_to_special(&self) -> bool {
        matches!(
            self,
            Self::Heaven | Self::Earth | Self::Invasive | Self::Extern
        )
    }
}

fn target_from_chunk_name(chunk_name: &str) -> PortalTarget {
    match chunk_name {
        "" => PortalTarget::Nothing,
        "heaven" => PortalTarget::Heaven,
        "earth" => PortalTarget::Earth,
        "invasive" => PortalTarget::Invasive,
        "extern" => PortalTarget::Extern,
        name => PortalTarget::Chunk(name.to_string()),
    }
}

/// A portal: a polygon in a boundary plane plus the chunk visible through it
/// (`ChunkBoundary::Portal`, `chunk_boundary.hpp:181-331`). `points` are 2D in
/// the portal's u/v basis; the 3D object-space position of point `i` is
/// `u_axis * u + v_axis * v + origin`. All vectors are chunk-local; `centre`
/// only becomes world space when the owning chunk binds
/// (`chunk.cpp:3339`: `centre = transform.applyPoint(lcentre)`).
#[derive(Debug, Clone, PartialEq)]
pub struct Portal {
    /// `<internal>`: the portal plane lies inside another chunk.
    pub internal: bool,
    /// `<permissive>` (default true): objects may pass through.
    pub permissive: bool,
    pub target: PortalTarget,
    /// The portal section's own string value (`label` in the reference).
    pub label: String,
    pub points: Vec<Vector2>,
    /// `<uAxis>`; the u basis vector of the portal plane.
    pub u_axis: Vector3,
    /// `plane.normal().crossProduct(uAxis)` — derived, never read from file.
    pub v_axis: Vector3,
    /// The closest point of the plane to the local origin:
    /// `normal * d / |normal|^2`.
    pub origin: Vector3,
    /// Local centre: `uAxis * avg.x + vAxis * avg.y + origin`.
    pub lcentre: Vector3,
    /// World centre; equals `lcentre` until the owning chunk binds.
    pub centre: Vector3,
    pub plane: PlaneEq,
}

impl Portal {
    /// The DataSection constructor (`chunk_boundary.cpp:358-475`). `plane` is
    /// the owning boundary's plane.
    pub fn from_section(section: &DataSection, plane: PlaneEq) -> Self {
        let label = section.as_string().to_string();
        let internal = section
            .find_child("internal")
            .is_some_and(|s| s.as_bool(false));
        let permissive = !section
            .find_child("permissive")
            .is_some_and(|s| !s.as_bool(true));
        let chunk_name = section.read_string("chunk", "");
        let target = target_from_chunk_name(&chunk_name);
        let u_axis = section.read_vector3("uAxis", Vector3::ZERO);

        let mut points = Vec::new();
        for i in 0..section.count_children() {
            if let Some(child) = section.open_child(i)
                && child.section_name() == "point"
            {
                let v = child.as_vector3(Vector3::ZERO);
                points.push(Vector2::new(v.x, v.y));
            }
        }
        Self::from_parts(plane, u_axis, points, target, internal, permissive, label)
    }

    /// The fast constructor (`chunk_boundary.cpp:491-595`): used by
    /// `create_boundaries` for the synthetic outside boundaries, which pass
    /// 3D points whose x/y become the 2D u/v coordinates.
    pub fn from_plane_points(
        plane: PlaneEq,
        u_axis: Vector3,
        points_3d: &[Vector3],
        chunk_name: &str,
    ) -> Self {
        let points = points_3d.iter().map(|p| Vector2::new(p.x, p.y)).collect();
        Self::from_parts(
            plane,
            u_axis,
            points,
            target_from_chunk_name(chunk_name),
            false,
            true,
            String::new(),
        )
    }

    /// The shared tail of both constructors: derive the v axis, origin and
    /// centre from the plane, then apply the winding check
    /// (`chunk_boundary.cpp:424-474`).
    fn from_parts(
        plane: PlaneEq,
        u_axis: Vector3,
        mut points: Vec<Vector2>,
        target: PortalTarget,
        internal: bool,
        permissive: bool,
        label: String,
    ) -> Self {
        let normal = *plane.normal();
        // `vAxis = plane.normal().crossProduct(uAxis)` and
        // `origin = normal * d / |normal|^2` (`chunk_boundary.cpp:426-427`).
        let v_axis = Vector3::ZERO.cross_product(&normal, &u_axis);
        let origin = normal * (plane.d() / normal.length_squared());

        let mut avg = Vector2::ZERO;
        for p in &points {
            avg.x += p.x;
            avg.y += p.y;
        }
        if !points.is_empty() {
            avg.x /= points.len() as f32;
            avg.y /= points.len() as f32;
        }
        let lcentre = u_axis * avg.x + v_axis * avg.y + origin;

        // Winding check: the polygon must run counter-clockwise seen from the
        // plane's front; a reversed polygon is flipped in place.
        if points.len() >= 3 {
            let p = |i: usize| -> Vector3 { u_axis * points[i].x + v_axis * points[i].y + origin };
            let mut test = PlaneEq::default();
            test.init(&p(0), &p(1), &p(2));
            let mut n1 = normal;
            n1.normalise();
            let n2 = *test.normal();
            if (n1 + n2).length() < 1.0 {
                points.reverse();
            }
        }

        Self {
            internal,
            permissive,
            target,
            label,
            points,
            u_axis,
            v_axis,
            origin,
            lcentre,
            centre: lcentre,
            plane,
        }
    }

    /// `project` (`chunk_boundary.cpp:676-680`): a local 3D point onto the
    /// portal's 2D basis.
    pub fn project(&self, point: &Vector3) -> Vector2 {
        let rel = *point - self.origin;
        Vector2::new(rel.dot_product(&self.u_axis), rel.dot_product(&self.v_axis))
    }

    /// `inside(Vector2)` (`chunk_boundary.cpp:700-716`): convex containment
    /// by edge normals, winding-sensitive.
    pub fn inside_2d(&self, point: &Vector2) -> bool {
        for i in 0..self.points.len() {
            let p1 = &self.points[i];
            let p2 = &self.points[(i + 1) % self.points.len()];
            let diff = Vector2::new(p1.y - p2.y, p2.x - p1.x);
            let rel = Vector2::new(point.x - p1.x, point.y - p1.y);
            if diff.dot_product(&rel) < 0.0 {
                return false;
            }
        }
        true
    }

    /// `inside(Vector3)` (`chunk_boundary.cpp:687-691`): the point is assumed
    /// to already lie on the portal plane.
    pub fn inside(&self, point: &Vector3) -> bool {
        self.inside_2d(&self.project(point))
    }

    /// `objectSpacePoint` (`chunk_boundary.cpp:749-752`).
    pub fn object_space_point(&self, i: usize) -> Vector3 {
        self.u_axis * self.points[i].x + self.v_axis * self.points[i].y + self.origin
    }
}

/// One boundary plane of a chunk and the portals in it
/// (`ChunkBoundary`, `chunk_boundary.hpp:139-348`). Portals to earth are
/// born bound; everything else starts unbound and is bound by the chunk
/// manager (BW-10-01-03).
#[derive(Debug, Clone, PartialEq)]
pub struct ChunkBoundary {
    pub plane: PlaneEq,
    pub bound_portals: Vec<Portal>,
    pub unbound_portals: Vec<Portal>,
}

impl ChunkBoundary {
    /// The DataSection constructor (`chunk_boundary.cpp:59-104`). A missing
    /// section yields the zero plane like the reference; a non-unit normal is
    /// normalised with `d` scaled to match.
    pub fn from_section(section: &DataSection) -> Self {
        let mut normal = section.read_vector3("normal", Vector3::ZERO);
        let d = section.find_child("d").map_or(0.0, |s| s.as_float(0.0));
        let len = normal.length();
        if len != 0.0 {
            normal = normal * (1.0 / len);
        }
        let d = d / len;
        let plane = PlaneEq::new(normal, d);

        let mut bound_portals = Vec::new();
        let mut unbound_portals = Vec::new();
        for i in 0..section.count_children() {
            if let Some(child) = section.open_child(i)
                && child.section_name() == "portal"
            {
                let portal = Portal::from_section(child, plane);
                if portal.target == PortalTarget::Earth {
                    bound_portals.push(portal);
                } else {
                    unbound_portals.push(portal);
                }
            }
        }
        Self {
            plane,
            bound_portals,
            unbound_portals,
        }
    }

    /// The plane+portal constructor (`chunk_boundary.cpp:115-144`).
    pub fn with_portal(plane: PlaneEq, portal: Portal) -> Self {
        let mut boundary = Self {
            plane,
            bound_portals: Vec::new(),
            unbound_portals: Vec::new(),
        };
        if portal.target == PortalTarget::Earth {
            boundary.bound_portals.push(portal);
        } else {
            boundary.unbound_portals.push(portal);
        }
        boundary
    }
}

/// The six boundaries of an outside chunk (`createBoundaries`,
/// `chunk.cpp:695-806`), in the reference order right, left, bottom, top,
/// back, front. Direction names follow the reference even where they read
/// inverted: "right" is the local `x = 0` face and connects to the chunk at
/// `(x-1, z)`; "left" is `x = gridSize` and connects to `(x+1, z)`.
pub fn create_boundaries(identifier: &str, mapping: &GeometryMapping) -> Vec<ChunkBoundary> {
    let Some((x, z)) = grid_from_chunk_name(identifier) else {
        return Vec::new();
    };
    let grid_size = mapping.grid_size;
    let min_y = MIN_CHUNK_HEIGHT;
    let max_y = MAX_CHUNK_HEIGHT;

    // (plane, uAxis, points, neighbour grid cell or the extern guard)
    struct Face {
        plane: PlaneEq,
        u_axis: Vector3,
        points: [Vector3; 4],
        chunk_name: String,
    }
    let v = |x: f32, y: f32, z: f32| Vector3::new(x, y, z);
    let mut faces: Vec<Face> = Vec::with_capacity(6);

    // right: plane x = 0, u = +y, to (x-1, z) or extern at minLGridX.
    faces.push(Face {
        plane: PlaneEq::new(v(1.0, 0.0, 0.0), 0.0),
        u_axis: v(0.0, 1.0, 0.0),
        points: [
            v(min_y, 0.0, 0.0),
            v(max_y, 0.0, 0.0),
            v(max_y, grid_size, 0.0),
            v(min_y, grid_size, 0.0),
        ],
        chunk_name: if i32::from(x) != mapping.bounds.min_l_grid_x {
            mapping.outside_chunk_identifier(i32::from(x) - 1, i32::from(z), false)
        } else {
            "extern".to_string()
        },
    });
    // left: plane x = gridSize, u = +z, to (x+1, z) or extern at maxLGridX.
    faces.push(Face {
        plane: PlaneEq::new(v(-1.0, 0.0, 0.0), -grid_size),
        u_axis: v(0.0, 0.0, 1.0),
        points: [
            v(0.0, min_y, 0.0),
            v(grid_size, min_y, 0.0),
            v(grid_size, max_y, 0.0),
            v(0.0, max_y, 0.0),
        ],
        chunk_name: if i32::from(x) != mapping.bounds.max_l_grid_x {
            mapping.outside_chunk_identifier(i32::from(x) + 1, i32::from(z), false)
        } else {
            "extern".to_string()
        },
    });
    // bottom: plane y = MIN_CHUNK_HEIGHT, to earth.
    faces.push(Face {
        plane: PlaneEq::new(v(0.0, 1.0, 0.0), min_y),
        u_axis: v(0.0, 0.0, 1.0),
        points: [
            v(0.0, 0.0, 0.0),
            v(grid_size, 0.0, 0.0),
            v(grid_size, grid_size, 0.0),
            v(0.0, grid_size, 0.0),
        ],
        chunk_name: "earth".to_string(),
    });
    // top: plane y = MAX_CHUNK_HEIGHT, to heaven.
    faces.push(Face {
        plane: PlaneEq::new(v(0.0, -1.0, 0.0), -max_y),
        u_axis: v(1.0, 0.0, 0.0),
        points: [
            v(0.0, 0.0, 0.0),
            v(grid_size, 0.0, 0.0),
            v(grid_size, grid_size, 0.0),
            v(0.0, grid_size, 0.0),
        ],
        chunk_name: "heaven".to_string(),
    });
    // back: plane z = 0, u = +x, to (x, z-1) or extern at minLGridY.
    faces.push(Face {
        plane: PlaneEq::new(v(0.0, 0.0, 1.0), 0.0),
        u_axis: v(1.0, 0.0, 0.0),
        points: [
            v(0.0, min_y, 0.0),
            v(grid_size, min_y, 0.0),
            v(grid_size, max_y, 0.0),
            v(0.0, max_y, 0.0),
        ],
        chunk_name: if i32::from(z) != mapping.bounds.min_l_grid_y {
            mapping.outside_chunk_identifier(i32::from(x), i32::from(z) - 1, false)
        } else {
            "extern".to_string()
        },
    });
    // front: plane z = gridSize, u = +y, to (x, z+1) or extern at maxLGridY.
    faces.push(Face {
        plane: PlaneEq::new(v(0.0, 0.0, -1.0), -grid_size),
        u_axis: v(0.0, 1.0, 0.0),
        points: [
            v(min_y, 0.0, 0.0),
            v(max_y, 0.0, 0.0),
            v(max_y, grid_size, 0.0),
            v(min_y, grid_size, 0.0),
        ],
        chunk_name: if i32::from(z) != mapping.bounds.max_l_grid_y {
            mapping.outside_chunk_identifier(i32::from(x), i32::from(z) + 1, false)
        } else {
            "extern".to_string()
        },
    });

    faces
        .into_iter()
        .map(|face| {
            let portal =
                Portal::from_plane_points(face.plane, face.u_axis, &face.points, &face.chunk_name);
            ChunkBoundary::with_portal(face.plane, portal)
        })
        .collect()
}

/// The inside-chunk boundary path (`createBoundaries`'s else branch,
/// `chunk.cpp:808-850`): the shell model's `.visual` (then `.static.visual`)
/// resource supplies `<boundary>` sections. `visual_resolver` loads that
/// resource's section, standing in for `BWResource::openSection`.
pub fn create_boundaries_from_shell(
    section: &DataSection,
    visual_resolver: &dyn Fn(&str) -> Option<DataSection>,
) -> Vec<ChunkBoundary> {
    let Some(shell) = section
        .find_child("shell")
        .or_else(|| first_model_section(section))
    else {
        return Vec::new();
    };
    let resource = shell.read_string("resource", "");
    if resource.is_empty() {
        return Vec::new();
    }
    let visual_name = resource
        .strip_suffix(".model")
        .map_or_else(|| resource.clone(), |stem| format!("{stem}.visual"));
    let visual = visual_resolver(&visual_name).or_else(|| {
        visual_resolver(&format!(
            "{}.static.visual",
            visual_name.strip_suffix(".visual").unwrap_or(&visual_name)
        ))
    });
    let Some(visual) = visual else {
        return Vec::new();
    };
    let mut boundaries = Vec::new();
    for i in 0..visual.count_children() {
        if let Some(child) = visual.open_child(i)
            && child.section_name() == "boundary"
        {
            boundaries.push(ChunkBoundary::from_section(child));
        }
    }
    boundaries
}

fn first_model_section(section: &DataSection) -> Option<&DataSection> {
    (0..section.count_children())
        .filter_map(|i| section.open_child(i))
        .find(|child| child.section_name() == "model")
}

/// `Chunk::formBoundaries` (`chunk.cpp:856-908`): build the boundary list and
/// classify each into `joints` (passable, with portals) and `bounds`
/// (containing planes). Returns `(boundaries, joint indices, bound indices,
/// good)`; `good` requires at least four boundaries and no degenerate plane.
pub fn form_boundaries(
    identifier: &str,
    section: &DataSection,
    mapping: &GeometryMapping,
    visual_resolver: &dyn Fn(&str) -> Option<DataSection>,
) -> (Vec<ChunkBoundary>, Vec<usize>, Vec<usize>, bool) {
    let boundaries = if is_outside_chunk(identifier) {
        create_boundaries(identifier, mapping)
    } else {
        create_boundaries_from_shell(section, visual_resolver)
    };

    let mut joints = Vec::new();
    let mut bounds = Vec::new();
    let mut good = boundaries.len() >= 4;
    for (i, boundary) in boundaries.iter().enumerate() {
        if boundary.plane.normal().length() == 0.0 {
            good = false;
            continue;
        }
        let mut isa_bound = false;
        let mut isa_joint = false;
        if !boundary.unbound_portals.is_empty() {
            isa_joint = true;
            if !boundary.unbound_portals[0].internal {
                // Only the first portal is checked: chunks are convex, so if
                // any portal is non-internal the boundary is a bound.
                isa_bound = true;
            }
        } else {
            // The only portals born bound connect to heaven or earth.
            if !boundary.bound_portals.is_empty() {
                isa_joint = true;
            }
            isa_bound = true;
        }
        if isa_bound {
            bounds.push(i);
        }
        if isa_joint {
            joints.push(i);
        }
    }
    (boundaries, joints, bounds, good)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{GeometryMapping, GridBounds};
    use resources::datasection;

    fn identity_mapping() -> GeometryMapping {
        GeometryMapping::new(
            "space/",
            100.0,
            GridBounds {
                min_l_grid_x: 0,
                min_l_grid_y: 0,
                max_l_grid_x: 1,
                max_l_grid_y: 1,
                min_grid_x: 0,
                min_grid_y: 0,
                max_grid_x: 1,
                max_grid_y: 1,
            },
            false,
        )
    }

    fn parse(xml: &str) -> DataSection {
        datasection::parse("", xml.as_bytes()).unwrap()
    }

    #[test]
    fn target_kinds_and_flags_cover_every_variant() {
        // `hasChunk` / `isConnectingToSpecial` for every target.
        assert!(!PortalTarget::Nothing.has_chunk());
        assert!(!PortalTarget::Nothing.is_connecting_to_special());
        assert!(PortalTarget::Chunk("00000000o".into()).has_chunk());
        assert!(!PortalTarget::Chunk("00000000o".into()).is_connecting_to_special());
        for t in [
            PortalTarget::Heaven,
            PortalTarget::Earth,
            PortalTarget::Invasive,
            PortalTarget::Extern,
        ] {
            assert!(!t.has_chunk());
            assert!(t.is_connecting_to_special());
        }
        // The empty and invasive spellings.
        assert_eq!(target_from_chunk_name(""), PortalTarget::Nothing);
        assert_eq!(target_from_chunk_name("invasive"), PortalTarget::Invasive);
        assert_eq!(
            target_from_chunk_name("00000001i"),
            PortalTarget::Chunk("00000001i".into())
        );
    }

    #[test]
    fn portal_section_reads_permissive_and_empty_points() {
        let plane = PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 10.0);
        let xml = r#"
            <portal>
                <internal>true</internal>
                <permissive>false</permissive>
                <chunk>00000001i</chunk>
                <uAxis>1 0 0</uAxis>
            </portal>"#;
        let portal = Portal::from_section(&parse(xml), plane);
        assert!(portal.internal);
        assert!(!portal.permissive);
        assert_eq!(portal.target, PortalTarget::Chunk("00000001i".into()));
        // No <point> children: the portal is empty, the centre is the origin.
        assert!(portal.points.is_empty());
        assert_eq!(portal.lcentre, portal.origin);

        // `objectSpacePoint` and the projection/containment round-trip.
        let xml = r#"
            <portal>
                <chunk>00000000o</chunk>
                <uAxis>1 0 0</uAxis>
                <point>-10 -10 0</point>
                <point>10 -10 0</point>
                <point>10 10 0</point>
                <point>-10 10 0</point>
            </portal>"#;
        let portal = Portal::from_section(&parse(xml), plane);
        assert_eq!(
            portal.object_space_point(0),
            Vector3::new(-10.0, -10.0, 10.0)
        );
        let corner = portal.object_space_point(0);
        assert!(portal.inside(&corner));
        assert!(!portal.inside(&Vector3::new(100.0, 0.0, 10.0)));
        assert!(portal.inside_2d(&portal.project(&corner)));
        assert!(!portal.inside_2d(&Vector2::new(100.0, 0.0)));
    }

    #[test]
    fn boundary_section_normalises_a_non_unit_normal() {
        // normal (2,0,0), d=20: normalised to (1,0,0), d=10.
        let xml = r#"
            <boundary>
                <normal>2 0 0</normal>
                <d>20</d>
                <portal><chunk>earth</chunk><uAxis>0 1 0</uAxis></portal>
            </boundary>"#;
        let boundary = ChunkBoundary::from_section(&parse(xml));
        assert_eq!(boundary.plane.normal(), &Vector3::new(1.0, 0.0, 0.0));
        assert_eq!(boundary.plane.d(), 10.0);
        // Earth portals are born bound.
        assert_eq!(boundary.bound_portals.len(), 1);
        assert!(boundary.unbound_portals.is_empty());
    }

    #[test]
    fn shell_falls_back_to_first_model_and_static_visual() {
        // No <shell>: the first <model> is the shell; the primary visual is
        // missing so the .static.visual fallback supplies the boundaries.
        let xml = r#"
            <00000000i.chunk>
                <model>
                    <resource>old.model</resource>
                </model>
            </00000000i.chunk>"#;
        let visual = r#"
            <old.static.visual>
                <boundary>
                    <normal>1 0 0</normal>
                    <d>10</d>
                </boundary>
                <boundary>
                    <normal>-1 0 0</normal>
                    <d>10</d>
                </boundary>
                <boundary>
                    <normal>0 1 0</normal>
                    <d>10</d>
                </boundary>
                <boundary>
                    <normal>0 -1 0</normal>
                    <d>10</d>
                </boundary>
            </old.static.visual>"#;
        let section = parse(xml);
        let resolver = |name: &str| (name == "old.static.visual").then(|| parse(visual));
        let boundaries = create_boundaries_from_shell(&section, &resolver);
        assert_eq!(boundaries.len(), 4);

        // No visual at all: no boundaries, like the reference.
        let none = create_boundaries_from_shell(&section, &|_| None);
        assert!(none.is_empty());

        // A shell with no resource and a section with no model: empty.
        let empty =
            parse(r#"<00000000i.chunk><model><resource></resource></model></00000000i.chunk>"#);
        assert!(create_boundaries_from_shell(&empty, &|_| None).is_empty());
        let no_model = parse(r#"<00000000i.chunk></00000000i.chunk>"#);
        assert!(create_boundaries_from_shell(&no_model, &|_| None).is_empty());
    }

    #[test]
    fn form_boundaries_rejects_a_degenerate_plane() {
        // An inside chunk whose shell visual has a zero-normal boundary:
        // `good` drops to false even with four planes (chunk.cpp:867-873).
        let visual = r#"
            <bad.visual>
                <boundary>
                    <normal>0 0 0</normal>
                    <d>1</d>
                </boundary>
                <boundary><normal>1 0 0</normal><d>1</d></boundary>
                <boundary><normal>-1 0 0</normal><d>1</d></boundary>
                <boundary><normal>0 1 0</normal><d>1</d></boundary>
                <boundary><normal>0 -1 0</normal><d>1</d></boundary>
            </bad.visual>"#;
        let section = parse(
            r#"<00000000i.chunk><shell><resource>bad.model</resource></shell></00000000i.chunk>"#,
        );
        let resolver = |name: &str| (name == "bad.visual").then(|| parse(visual));
        let (boundaries, _joints, _bounds, good) =
            form_boundaries("00000000i", &section, &identity_mapping(), &resolver);
        assert_eq!(boundaries.len(), 5);
        assert!(!good, "zero normal fails the form");
    }
}
