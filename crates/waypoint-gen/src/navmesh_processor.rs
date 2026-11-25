//! BW-10-05-04 remainder: the editor's per-girth workflow ring —
//! `tools/common/navmesh_processor.cpp` (`processInBackground`,
//! `:47-102`) plus the flood entry it drives
//! (`tools/common/chunk_flooder.cpp`, `flood`, `:274-409`) and the
//! girth specs it loops over (`tools/common/girth.cpp`).
//!
//! The navgen per-chunk face (BW-10-07-02) runs one girth through
//! flood -> generate -> asBinary. The editor's background processor is
//! the same face in a **loop over the space's girths**: per girth it
//! checks the abort flag, floods fresh into the same flooder, copies
//! the 16 height/adjacency layers into the generator, and appends the
//! girth's `asBinary` record into one navmesh blob that the cache
//! later saves as the chunk's `.cdata/worldNavmesh` section
//! (see [`crate::navmesh_cache`]).

use crate::flood::{FloodChunk, FloodPhysics, MAX_HEIGHTS, WaypointFlood};
use crate::generator::{GenChunk, WaypointGenerator};
use crate::view::as_binary;
use foundation::math::Vector3;

/// One `<girth>` section of `helpers/girths.xml`
/// (`tools/common/girth.cpp:13-28`).
#[derive(Clone, Debug)]
pub struct GirthSection {
    /// The section's own float — `girth_ = ds->asFloat()`.
    pub value: f32,
    /// `width` — required.
    pub width: f32,
    /// `height` — required.
    pub height: f32,
    /// `depth` — required, zero allowed.
    pub depth: f32,
    /// `maxSlope` — defaults to 45.
    pub max_slope: Option<f32>,
    /// `maxClimb` — defaults to `height / 3`.
    pub max_climb: Option<f32>,
    /// `always` — the section's presence.
    pub always: bool,
}

/// `Girth` (`tools/common/girth.hpp:13-32`): one girth spec the
/// processor loops over. The constructor's validation is the
/// reference's `CRITICAL_MSG` (`girth.cpp:23-27`): a non-positive
/// width or height, or a negative depth, is fatal.
#[derive(Clone, Debug, PartialEq)]
pub struct Girth {
    /// `girth_` — the value written into the navmesh record header.
    pub girth: f32,
    /// `width_`.
    pub width: f32,
    /// `height_`.
    pub height: f32,
    /// `depth_`.
    pub depth: f32,
    /// `maxSlope_`.
    pub max_slope: f32,
    /// `maxClimb_`.
    pub max_climb: f32,
    /// `always_`.
    pub always: bool,
}

impl Girth {
    /// `Girth::Girth(DataSectionPtr)` (`girth.cpp:13-28`): apply the
    /// defaults, then validate.
    pub fn from_section(section: &GirthSection) -> Girth {
        let girth = Girth {
            girth: section.value,
            width: section.width,
            height: section.height,
            depth: section.depth,
            max_slope: section.max_slope.unwrap_or(45.0),
            max_climb: section.max_climb.unwrap_or(section.height / 3.0),
            always: section.always,
        };
        assert!(
            girth.width > 0.0 && girth.height > 0.0 && girth.depth >= 0.0,
            "The girth settings (width: {}, height: {}, depth_: {}) is incorrect.",
            girth.width,
            girth.height,
            girth.depth
        );
        girth
    }

    /// `radius()` (`girth.hpp:22`).
    pub fn radius(&self) -> f32 {
        self.width.min(self.depth) * 0.5
    }
}

/// The per-girth physics handler source. The reference constructs a
/// fresh `PhysicsHandler(space, girth)` per girth inside the flood and
/// another for the edge determination (`chunk_flooder.cpp:283`,
/// `navmesh_processor.cpp:63`); the port hands out the handler that
/// carries the girth spec's dimensions.
pub trait GirthPhysicsSource {
    /// The handler for `girth`.
    fn physics_for(&mut self, girth: &Girth) -> &mut dyn FloodPhysics;
}

/// The flood entry the ring drives per girth
/// (`chunk_flooder.cpp:274-409`): `reset` (`:98-111`, resolution 0.5,
/// the chunk's bounding box widened by two resolutions each way), the
/// centre seed appended after the portal seeds, then either the flash
/// flood (`flashFlood`, `:417-480`) or the real fill loop over the
/// seeds and the entity points (`:300-353`), and the two postfilters
/// plus any shrinks (`:383-398`).
///
/// Boundary note: the flash-flood gate itself reads chunk-space column
/// state (holdings, terrain height/hole maps, `:417-474`) — the
/// chunk-space domain this crate does not model. The caller resolves
/// that gate and passes the outcome: `Some(wpHeight)` floods flat at
/// that height, `None` runs the real fill. The write-TGA arms are
/// omitted: the processor passes `writeTGAs = false`
/// (`navmesh_processor.cpp:70`).
///
/// `progress` is `NULL` in the ring (`navmesh_processor.cpp:70`), so
/// the fill's `-1` abort arm is unreachable here and is not carried;
/// the only stop channel is the per-seed `shouldAbortTask` check
/// (`chunk_flooder.cpp:319-322`).
#[allow(clippy::too_many_arguments)]
pub fn flood_chunk(
    flood: &mut WaypointFlood,
    chunk_bb: (&Vector3, &Vector3),
    chunk: &dyn FloodChunk,
    physics: &mut dyn FloodPhysics,
    seeds: &[Vector3],
    entity_pts: &[Vector3],
    flash_flood_height: Option<f32>,
    should_abort: &dyn Fn() -> bool,
    nshrink: i32,
) -> bool {
    // reset() (`chunk_flooder.cpp:98-111`).
    let reso = 0.5f32;
    let widen = Vector3::new(reso * 2.0, 0.0, reso * 2.0);
    flood.set_area(&(*chunk_bb.0 - widen), &(*chunk_bb.1 + widen), reso);

    // getSeedPoints appends the centre seed last
    // (`chunk_flooder.cpp:226-237`); flashFlood floods from exactly
    // that seed (`:294`).
    let centre = (*flood.min() + *flood.max()) * 0.5;
    let mut seed_pts = seeds.to_vec();
    seed_pts.push(centre);

    match flash_flood_height {
        Some(wp_height) => {
            flood.flash_flood(wp_height);
        }
        None => {
            // the entity points join the seeds only on the real-flood
            // arm (`chunk_flooder.cpp:302-307` and its comment)
            seed_pts.extend_from_slice(entity_pts);
            for seed in &seed_pts {
                if should_abort() {
                    return false;
                }
                flood.fill(seed, physics, chunk, None);
            }
        }
    }

    flood.postfilter_add();
    flood.postfilter_remove();
    for _ in 0..nshrink {
        flood.shrink();
    }
    true
}

/// `NavmeshProcessor::processInBackground` (`navmesh_processor.cpp:47-102`):
/// loop the girths, and per girth flood the chunk fresh, regenerate the
/// polygons, and append the girth's binary record into `navmesh`. The
/// blob is what the cache saves (`navmesh_processor.cpp:96-97`,
/// `:111-112`).
///
/// `physics_of` builds the per-girth physics handler — the reference
/// constructs a fresh `PhysicsHandler(space, girth)` inside the flood
/// and another for the edge determination (`chunk_flooder.cpp:283`,
/// `navmesh_processor.cpp:63`).
///
/// `extendThroughUnboundPortals` (`:91`) is the registered no-op
/// (`waypoint_generator.cpp:2370-2607`): both fixed-map chunks are
/// bound, the stub `bounds()` is empty, and the reference call is a
/// no-op — the port omits the gate, as in the navgen face.
///
/// A flood failure or an abort returns `false`; the caller drops the
/// blob (`calcDone` keeps `dirty_` set when `backgroundTaskResult` is
/// false, `editor_chunk_navmesh_cache_base.cpp:341-364`).
#[allow(clippy::too_many_arguments)]
pub fn process_in_background(
    flood: &mut WaypointFlood,
    gener: &mut WaypointGenerator,
    girths: &[Girth],
    chunk_bb: (&Vector3, &Vector3),
    chunk: &(impl FloodChunk + GenChunk),
    chunk_identifier: &str,
    seeds: &[Vector3],
    entity_pts: &[Vector3],
    flash_flood_height: Option<f32>,
    physics_of: &mut dyn GirthPhysicsSource,
    should_abort: &dyn Fn() -> bool,
    nshrink: i32,
    navmesh: &mut Vec<u8>,
) -> bool {
    for girth in girths {
        // the abort check leads every girth (`:54-58`)
        if should_abort() {
            return false;
        }

        let physics = physics_of.physics_for(girth);

        if !flood_chunk(
            flood,
            chunk_bb,
            chunk,
            physics,
            seeds,
            entity_pts,
            flash_flood_height,
            should_abort,
            nshrink,
        ) {
            // the flood was stopped or failed (`:70-74`)
            return false;
        }

        // finish up: copy the 16 layers and regenerate (`:77-92`)
        let (w, h) = (flood.xsize(), flood.zsize());
        gener.init(w, h, flood.min(), flood.resolution());
        for g in 0..MAX_HEIGHTS {
            gener.adj_grids_mut()[g] = flood.adj_grids()[g].to_vec();
            gener.hgt_grids_mut()[g] = flood.hgt_grids()[g].to_vec();
        }

        gener.generate();
        gener.determine_edges_adjacent_to_other_chunks(chunk, physics);
        gener.streamline();
        gener.calculate_set_membership(entity_pts, chunk_identifier);

        // the record joins the blob with the girth that produced it
        // (`:94-97`)
        let record = as_binary(gener, girth.girth);
        navmesh.extend_from_slice(&record);
    }

    // success (`:101`)
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flood::tests::{Slab, grid, seed};
    use foundation::math::vector::Vector3 as V3;

    /// The chunk both flood and generator traits see: the reference
    /// passes the one `Chunk` to the flood and to
    /// `determineEdgesAdjacentToOtherChunks`; the port's two seams are
    /// one object here.
    struct RingChunk {
        owns_box: (f32, f32, f32, f32),
        space: (f32, f32, f32, f32),
        bb: (V3, V3),
    }

    impl RingChunk {
        fn everywhere(bb: (V3, V3)) -> Self {
            Self {
                owns_box: (-100.0, -100.0, 100.0, 100.0),
                space: (-100.0, 100.0, -100.0, 100.0),
                bb,
            }
        }
    }

    impl crate::flood::FloodChunk for RingChunk {
        fn owns(&self, point: &V3) -> bool {
            let (x0, z0, x1, z1) = self.owns_box;
            point.x >= x0 && point.x <= x1 && point.z >= z0 && point.z <= z1
        }
        fn min_coord_x(&self) -> f32 {
            self.space.0
        }
        fn max_coord_x(&self) -> f32 {
            self.space.1
        }
        fn min_coord_z(&self) -> f32 {
            self.space.2
        }
        fn max_coord_z(&self) -> f32 {
            self.space.3
        }
    }

    impl GenChunk for RingChunk {
        fn owns(&self, point: &V3) -> bool {
            let (x0, z0, x1, z1) = self.owns_box;
            point.x >= x0 && point.x <= x1 && point.z >= z0 && point.z <= z1
        }
        fn local_bb(&self) -> (V3, V3) {
            self.bb
        }
    }

    /// The scene physics: the handler the reference builds per girth —
    /// its girth is the spec's dimensions (`physics_handler.cpp` ctor).
    struct ScenePhysics {
        slabs: Vec<Slab>,
        max_step: f32,
        girth_dims: V3,
    }

    impl ScenePhysics {
        fn top_at(&self, x: f32, z: f32, below: f32) -> Option<f32> {
            self.slabs
                .iter()
                .filter(|s| x >= s.0 && x < s.2 && z >= s.1 && z < s.3 && s.4 <= below)
                .map(|s| s.4)
                .fold(None, |best: Option<f32>, h| {
                    Some(best.map_or(h, |b: f32| b.max(h)))
                })
        }
    }

    impl FloodPhysics for ScenePhysics {
        fn get_girth(&self) -> V3 {
            self.girth_dims
        }

        fn get_scramble_height(&self) -> f32 {
            0.5
        }

        fn find_drop_point(&mut self, pos: &V3, y: &mut f32) -> bool {
            match self.top_at(pos.x, pos.z, pos.y) {
                Some(h) => {
                    *y = h;
                    true
                }
                None => false,
            }
        }

        fn adjust_move(&mut self, src: &V3, dst: &V3, dst2: &mut V3) {
            match self.top_at(dst.x, dst.z, dst.y + self.get_scramble_height()) {
                Some(h) if (h - src.y).abs() <= self.max_step => {
                    *dst2 = V3::new(dst.x, h, dst.z);
                }
                _ => {
                    *dst2 = V3::new(dst.x, -10000.0, dst.z);
                }
            }
        }
    }

    /// The per-girth source: one scene handler whose girth dims follow
    /// the requested spec, like `PhysicsHandler(space, girth)`.
    struct PerGirthPhysics {
        physics: ScenePhysics,
    }

    impl GirthPhysicsSource for PerGirthPhysics {
        fn physics_for(&mut self, g: &Girth) -> &mut dyn FloodPhysics {
            self.physics.girth_dims = V3::new(g.width, g.height, g.depth);
            &mut self.physics
        }
    }

    /// The slope scene's slab list, reused across girths.
    fn slope_slabs() -> Vec<Slab> {
        vec![
            Slab(0.0, 0.0, 3.0, 10.0, 0.0),
            Slab(3.0, 0.0, 7.0, 10.0, 1.5),
            Slab(7.0, 0.0, 10.0, 10.0, 3.0),
        ]
    }

    fn girth(value: f32) -> Girth {
        Girth::from_section(&GirthSection {
            value,
            width: value,
            height: 1.5,
            depth: value,
            max_slope: None,
            max_climb: None,
            always: false,
        })
    }

    /// Sequential version-0 record walk, the same arithmetic as the
    /// reader's `parseOldParts` (`waypoint_view.cpp:197-218`): each
    /// record is the 16-byte header, `numWaypoints` 12-byte waypoint
    /// headers (minH/maxH/vertexCount), and `numEdges` 12-byte vertex
    /// entries (x/z/adjacency) — `numEdges` is the total vertex count.
    fn parse_records(blob: &[u8]) -> Vec<(i32, f32, i32, i32)> {
        let mut records = Vec::new();
        let mut at = 0usize;
        while at < blob.len() {
            assert!(at + 16 <= blob.len(), "record header truncated");
            let read_i32 = |a: usize| i32::from_le_bytes(blob[a..a + 4].try_into().unwrap());
            let version = read_i32(at);
            let record_girth = f32::from_le_bytes(blob[at + 4..at + 8].try_into().unwrap());
            let num_waypoints = read_i32(at + 8);
            let num_edges = read_i32(at + 12);
            assert_eq!(version, 0, "version-0 record");
            at += 16 + (num_waypoints as usize) * 12 + (num_edges as usize) * 12;
            assert!(at <= blob.len(), "record stream truncated");
            records.push((version, record_girth, num_waypoints, num_edges));
        }
        records
    }

    /// One girth through the ring on the slope scene, standalone: the
    /// blob the ring produces for that girth alone.
    fn single_girth_run(value: f32) -> Vec<u8> {
        let bb = (V3::new(0.0, 0.0, 0.0), V3::new(10.0, 4.0, 10.0));
        let chunk = RingChunk::everywhere((bb.0, bb.1));
        let mut flood = grid(0.0, 10.0, 0.5);
        let mut gener = WaypointGenerator::new();
        let mut navmesh = Vec::new();
        let mut provider = PerGirthPhysics {
            physics: ScenePhysics {
                slabs: slope_slabs(),
                max_step: 0.6,
                girth_dims: V3::new(value, 1.5, value),
            },
        };
        let spec = girth(value);
        let ok = process_in_background(
            &mut flood,
            &mut gener,
            std::slice::from_ref(&spec),
            (&bb.0, &bb.1),
            &chunk,
            "slope",
            &[seed(0.5, 0.5, 0.5)],
            &[seed(5.0, 2.0, 5.0)],
            None,
            &mut provider,
            &|| false,
            0,
            &mut navmesh,
        );
        assert!(ok);
        navmesh
    }

    #[test]
    fn two_girths_produce_two_concatenated_records() {
        let bb = (V3::new(0.0, 0.0, 0.0), V3::new(10.0, 4.0, 10.0));
        let chunk = RingChunk::everywhere((bb.0, bb.1));
        let mut flood = grid(0.0, 10.0, 0.5);
        let mut gener = WaypointGenerator::new();
        let mut navmesh = Vec::new();
        let mut provider = PerGirthPhysics {
            physics: ScenePhysics {
                slabs: slope_slabs(),
                max_step: 0.6,
                girth_dims: V3::new(0.5, 1.5, 0.5),
            },
        };

        let ok = process_in_background(
            &mut flood,
            &mut gener,
            &[girth(0.5), girth(0.8)],
            (&bb.0, &bb.1),
            &chunk,
            "slope",
            &[seed(0.5, 0.5, 0.5)],
            &[seed(5.0, 2.0, 5.0)],
            None,
            &mut provider,
            &|| false,
            0,
            &mut navmesh,
        );
        assert!(ok);

        let records = parse_records(&navmesh);
        assert_eq!(records.len(), 2, "one record per girth");
        assert_eq!(records[0].1, 0.5, "first record carries its girth");
        assert_eq!(records[1].1, 0.8, "second record carries its girth");

        // each record is byte-for-byte the standalone run of the same
        // girth: the ring is the single face per girth, appended
        let single_narrow = single_girth_run(0.5);
        let single_wide = single_girth_run(0.8);
        assert_eq!(records[0], parse_records(&single_narrow)[0]);
        assert_eq!(records[1], parse_records(&single_wide)[0]);
        let split = parse_records(&navmesh);
        let _ = split;
    }

    #[test]
    fn the_record_bytes_equal_the_standalone_single_girth_run() {
        let bb = (V3::new(0.0, 0.0, 0.0), V3::new(10.0, 4.0, 10.0));
        let chunk = RingChunk::everywhere((bb.0, bb.1));
        let mut flood = grid(0.0, 10.0, 0.5);
        let mut gener = WaypointGenerator::new();
        let mut navmesh = Vec::new();
        let mut provider = PerGirthPhysics {
            physics: ScenePhysics {
                slabs: slope_slabs(),
                max_step: 0.6,
                girth_dims: V3::new(0.8, 1.5, 0.8),
            },
        };

        assert!(process_in_background(
            &mut flood,
            &mut gener,
            &[girth(0.8)],
            (&bb.0, &bb.1),
            &chunk,
            "slope",
            &[seed(0.5, 0.5, 0.5)],
            &[seed(5.0, 2.0, 5.0)],
            None,
            &mut provider,
            &|| false,
            0,
            &mut navmesh,
        ));

        assert_eq!(navmesh, single_girth_run(0.8), "ring record == standalone");
    }

    #[test]
    fn abort_between_girths_stops_the_ring() {
        let bb = (V3::new(0.0, 0.0, 0.0), V3::new(10.0, 4.0, 10.0));
        let chunk = RingChunk::everywhere((bb.0, bb.1));
        let mut flood = grid(0.0, 10.0, 0.5);
        let mut gener = WaypointGenerator::new();
        let mut navmesh = Vec::new();
        let mut provider = PerGirthPhysics {
            physics: ScenePhysics {
                slabs: slope_slabs(),
                max_step: 0.6,
                girth_dims: V3::new(0.5, 1.5, 0.5),
            },
        };
        let done_girths = std::cell::Cell::new(0usize);

        let ok = process_in_background(
            &mut flood,
            &mut gener,
            &[girth(0.5), girth(0.8), girth(1.0)],
            (&bb.0, &bb.1),
            &chunk,
            "slope",
            &[seed(0.5, 0.5, 0.5)],
            &[seed(5.0, 2.0, 5.0)],
            // the flash-flood arm performs no per-seed abort checks
            // (`chunk_flooder.cpp:319-322` are on the fill arm only),
            // so the counter below counts exactly one call per girth
            Some(2.0),
            &mut provider,
            &|| {
                done_girths.set(done_girths.get() + 1);
                done_girths.get() > 2 // stop at the third girth's lead
            },
            0,
            &mut navmesh,
        );
        assert!(!ok, "the abort flag ends the ring with false");

        // the first two girths appended their records before the stop
        let records = parse_records(&navmesh);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].1, 0.5);
        assert_eq!(records[1].1, 0.8);
    }

    #[test]
    fn abort_before_the_first_girth_produces_nothing() {
        let bb = (V3::new(0.0, 0.0, 0.0), V3::new(10.0, 4.0, 10.0));
        let chunk = RingChunk::everywhere((bb.0, bb.1));
        let mut flood = grid(0.0, 10.0, 0.5);
        let mut gener = WaypointGenerator::new();
        let mut navmesh = Vec::new();
        let mut provider = PerGirthPhysics {
            physics: ScenePhysics {
                slabs: slope_slabs(),
                max_step: 0.6,
                girth_dims: V3::new(0.5, 1.5, 0.5),
            },
        };

        let ok = process_in_background(
            &mut flood,
            &mut gener,
            &[girth(0.5)],
            (&bb.0, &bb.1),
            &chunk,
            "slope",
            &[seed(0.5, 0.5, 0.5)],
            &[seed(5.0, 2.0, 5.0)],
            None,
            &mut provider,
            &|| true,
            0,
            &mut navmesh,
        );
        assert!(!ok);
        assert!(navmesh.is_empty());
    }

    #[test]
    fn the_flash_flood_arm_skips_the_entity_points() {
        // the reference does not append entity points on the
        // flash-flood arm (`chunk_flooder.cpp:302-307` comment): a
        // doomed entity point cannot poison a flat flood
        let bb = (V3::new(0.0, 0.0, 0.0), V3::new(10.0, 4.0, 10.0));
        let chunk = RingChunk::everywhere((bb.0, bb.1));
        let mut flood = grid(0.0, 10.0, 0.5);
        let mut gener = WaypointGenerator::new();
        let mut navmesh = Vec::new();
        let mut provider = PerGirthPhysics {
            physics: ScenePhysics {
                slabs: slope_slabs(),
                max_step: 0.6,
                girth_dims: V3::new(0.5, 1.5, 0.5),
            },
        };

        let ok = process_in_background(
            &mut flood,
            &mut gener,
            &[girth(0.5)],
            (&bb.0, &bb.1),
            &chunk,
            "slope",
            &[seed(0.5, 0.5, 0.5)],
            // a point hanging in the void: on the fill arm it seeds
            // nothing and is harmless, but the flash-flood arm must
            // not even walk the list
            &[seed(5.0, -10000.0, 5.0)],
            Some(2.0),
            &mut provider,
            &|| false,
            0,
            &mut navmesh,
        );
        assert!(ok);
        let records = parse_records(&navmesh);
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn girth_defaults_and_validation_match_the_reference() {
        // defaults: maxSlope 45, maxClimb height/3 (`girth.cpp:19-20`)
        let spec = girth(0.6);
        assert_eq!(spec.max_slope, 45.0);
        assert!((spec.max_climb - 0.5).abs() < 1e-6);
        assert_eq!(spec.girth, 0.6);
        assert!((spec.radius() - 0.3).abs() < 1e-6);

        // explicit values pass through
        let explicit = Girth::from_section(&GirthSection {
            value: 1.0,
            width: 1.0,
            height: 2.0,
            depth: 0.0, // zero depth is allowed (`depth_ < 0.f` fails)
            max_slope: Some(30.0),
            max_climb: Some(0.25),
            always: true,
        });
        assert_eq!(explicit.max_slope, 30.0);
        assert_eq!(explicit.max_climb, 0.25);
        assert!(explicit.always);
        // radius = min(width, depth) * 0.5 (`girth.cpp`): the section
        // validates with depth 0, but the radius collapses to 0 too
        assert_eq!(explicit.radius(), 0.0);

        // non-positive width/height or negative depth is the
        // CRITICAL_MSG arm (`girth.cpp:23-27`)
        let bad_width = GirthSection {
            value: 0.0,
            width: 0.0,
            height: 1.5,
            depth: 0.6,
            max_slope: None,
            max_climb: None,
            always: false,
        };
        let bad_depth = GirthSection {
            value: 0.0,
            width: 0.6,
            height: 1.5,
            depth: -0.1,
            max_slope: None,
            max_climb: None,
            always: false,
        };
        for section in [&bad_width, &bad_depth] {
            let result = std::panic::catch_unwind(|| Girth::from_section(section));
            assert!(result.is_err(), "invalid girth settings must fail");
        }
    }
}
