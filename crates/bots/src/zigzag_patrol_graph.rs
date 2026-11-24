//! The zigzag patrol walker (`server/tools/bots/zigzag_patrol_graph.{hpp,cpp}`):
//! a [`crate::patrol_graph::GraphTraverser`]-shaped controller that
//! stays inside a corridor around the node path and zigzags across it.
//!
//! Reference quirks preserved:
//! - the constructor traverses with `randomise` and `snap` both set and
//!   takes its source position from the (possibly snap-moved) start
//!   (`zigzag_patrol_graph.cpp:26-40`);
//! - the final "Move!" divide is unguarded -- a zero-length zigzag
//!   vector with zero distance walks into `0/0`, a positive distance
//!   into infinity times zero (`zigzag_patrol_graph.cpp:131-134`);
//! - `almostEqual` (epsilon 0.0004) picks the vertical-line branch;
//! - the factory takes the *first* comma, the width parses through the
//!   C `atof` prefix rules, and a non-positive width falls back to the
//!   50 default (`zigzag_patrol_graph.cpp:166-178`).

use std::rc::Rc;

use foundation::math::Vector3;
use resources::bwresource::BwResource;

use crate::movement::{
    Direction3D, MovementController, MovementError, MovementErrorKind, MovementFactory, Rng,
    almost_equal, c_atof,
};
use crate::patrol_graph::{Graph, GraphTraverserCore, Graphs};

/// `ZigzagGraphTraverser::DEFAULT_CORRIDOR_WIDTH`
/// (`zigzag_patrol_graph.hpp:31`).
pub const DEFAULT_CORRIDOR_WIDTH: f32 = 50.0;

/// `ZigzagGraphTraverser` (`zigzag_patrol_graph.hpp:24-58`): the
/// traverser core plus the corridor bookkeeping.
pub struct ZigzagGraphTraverser {
    core: GraphTraverserCore,
    source_pos: Vector3,
    zigzag_pos: Vector3,
    corridor_width: f32,
}

impl ZigzagGraphTraverser {
    /// `ZigzagGraphTraverser::ZigzagGraphTraverser`
    /// (`zigzag_patrol_graph.cpp:26-40`): randomise + snap traversal,
    /// then the source is wherever the snap left the start.
    pub fn new(
        graph: Rc<Graph>,
        speed: &mut f32,
        start_position: &mut Vector3,
        corridor_width: f32,
        rng: &mut dyn Rng,
    ) -> Self {
        let core = GraphTraverserCore::new(graph, speed, start_position, true, true, rng);

        // The initial source position is the start position
        let source_pos = *start_position;

        let mut traverser = Self {
            core,
            source_pos,
            zigzag_pos: Vector3::ZERO,
            corridor_width,
        };

        // Set up first target
        traverser.set_zigzag_pos(&*start_position, rng);
        traverser
    }

    /// `ZigzagGraphTraverser::setZigzagPos`
    /// (`zigzag_patrol_graph.cpp:42-80`): the closest point on the
    /// source-destination line, then a point just across it.
    fn set_zigzag_pos(&mut self, curr_pos: &Vector3, rng: &mut dyn Rng) {
        // Path between nodes and currentPos
        let a = self.source_pos;
        let b = self.core.destination_pos;
        let p = *curr_pos;

        // Calculate closest point to position on line between a and b
        let poi = if !almost_equal(b.x, a.x) {
            let m = (b.z - a.z) / (b.x - a.x);
            let x = (p.x + m * p.z + m * m * a.x - m * a.z) / (m * m + 1.0);
            let z = m * (x - a.x) + a.z;
            Vector3::new(x, curr_pos.y, z)
        } else {
            // This bit prevents divide by zero in the above block
            Vector3::new(a.x, curr_pos.y, p.z)
        };

        // If the distance from the poi to the center of the destination
        // node is less than the corridor width, then set the zigzagPos
        // to be the destination position
        if (self.core.destination_pos - poi).length() < 2.0 * self.corridor_width {
            self.zigzag_pos = self.core.destination_pos;
            return;
        }

        // Unit vector on edge
        let mut along = b - a;
        along.normalise();

        // Unit vector from curr pos towards edge
        let mut across = poi - p;
        across.normalise();

        // Somewhere on the other side of the edge and forward
        let along_amt = 2.0 * rng.frand(self.corridor_width);
        let across_amt = rng.frand(self.corridor_width);
        self.zigzag_pos = poi + along * along_amt + across * across_amt;
    }
}

impl MovementController for ZigzagGraphTraverser {
    /// `ZigzagGraphTraverser::nextStep`
    /// (`zigzag_patrol_graph.cpp:82-139`).
    fn next_step(
        &mut self,
        speed: &mut f32,
        d_time: f32,
        pos: &mut Vector3,
        dir: &mut Direction3D,
        rng: &mut dyn Rng,
    ) -> bool {
        // Distance travelled per tick
        let distance = *speed * d_time;

        // The destination node
        let p_node = self.core.graph.node(self.core.destination_node);

        // Vectors to dest and zzp respectively
        let dest_vec = self.core.destination_pos - *pos;
        let zz_vec = self.zigzag_pos - *pos;

        // If we are already at a node
        if self.core.stay_time > 0.0 {
            self.core.stay_time -= d_time;

            // If our stay is over pick a new dest and zzp
            if self.core.stay_time <= 0.0 {
                self.source_pos = *p_node.position();
                self.core.destination_node = p_node.next_node(rng);
                let p_node = self.core.graph.node(self.core.destination_node);
                self.core.destination_pos = p_node.generate_destination(rng);
                self.set_zigzag_pos(pos, rng);
            }
            // If we're still hanging around but we've arrived at the
            // zzp, pick a new one
            else if zz_vec.length() < distance {
                self.zigzag_pos = p_node.generate_destination(rng);
            }
        }
        // If we have approached a node
        else if dest_vec.length() < distance {
            self.core.stay_time = p_node.generate_stay_time(rng);
            self.zigzag_pos = p_node.generate_destination(rng);
        }
        // If we are at a zzp
        else if zz_vec.length() < distance {
            self.set_zigzag_pos(pos, rng);
        }

        // Move!
        let zz_vec = self.zigzag_pos - *pos;
        *pos = *pos + zz_vec * (distance / zz_vec.length());
        dir.yaw = zz_vec.yaw();

        true
    }
}

/// `ZigzagPatrolFactory` (`zigzag_patrol_graph.cpp:157-216`): the
/// "ZigzagPatrol" factory; its graph cache lives as long as the
/// factory.
pub struct ZigzagPatrolFactory {
    graphs: Graphs,
}

impl ZigzagPatrolFactory {
    /// The constructor.
    pub fn new() -> Self {
        Self {
            graphs: Graphs::new(),
        }
    }
}

impl Default for ZigzagPatrolFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl MovementFactory for ZigzagPatrolFactory {
    /// `ZigzagPatrolFactory::create`
    /// (`zigzag_patrol_graph.cpp:170-202`): `graphName` or
    /// `graphName,corridorWidth`; the graph loads at unit scales and
    /// the width falls back to the default when not positive.
    fn create(
        &mut self,
        resource: &BwResource,
        data: &str,
        speed: &mut f32,
        position: &mut Vector3,
        rng: &mut dyn Rng,
    ) -> Result<Option<Box<dyn MovementController>>, MovementError> {
        let graph_name;
        let mut corridor_width = -1.0f32;

        match data.find(',') {
            Some(argpos) => {
                graph_name = data[..argpos].to_string();
                let arg2 = &data[argpos + 1..];
                corridor_width = c_atof(arg2);
            }
            None => graph_name = data.to_string(),
        }

        let Some(p_graph) = self.graphs.get_graph(resource, &graph_name, 1.0, 1.0) else {
            return Err(MovementError::new(
                MovementErrorKind::IoError,
                format!("Unable to load {graph_name}"),
            ));
        };

        if corridor_width > 0.0 {
            Ok(Some(Box::new(ZigzagGraphTraverser::new(
                p_graph,
                speed,
                position,
                corridor_width,
                rng,
            ))))
        } else {
            Ok(Some(Box::new(ZigzagGraphTraverser::new(
                p_graph,
                speed,
                position,
                DEFAULT_CORRIDOR_WIDTH,
                rng,
            ))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::Rng;

    /// A draw stream that hands out fixed values in rotation.
    struct FixedRng {
        values: Vec<u32>,
        at: usize,
    }

    impl FixedRng {
        fn new(values: &[u32]) -> Self {
            Self {
                values: values.to_vec(),
                at: 0,
            }
        }
    }

    impl Rng for FixedRng {
        fn next_u32(&mut self) -> u32 {
            let v = self.values[self.at % self.values.len()];
            self.at += 1;
            v
        }
    }

    /// Two nodes on the x axis, each its neighbour.
    fn two_node_graph() -> Rc<Graph> {
        let xml = "<root><nodes>\
                   <node><name>a</name><pos>0 0 0</pos><radius>0</radius>\
                   <edges><edge>b</edge></edges></node>\
                   <node><name>b</name><pos>100 0 0</pos><radius>0</radius>\
                   <edges><edge>a</edge></edges></node></nodes></root>";
        let section = resources::datasection::parse("root", xml.as_bytes()).unwrap();
        let mut graph = Graph::default();
        graph.init(&section, 1.0, 1.0).unwrap();
        Rc::new(graph)
    }

    /// Acceptance: the constructor traverses with randomise and snap
    /// set (`zigzag_patrol_graph.cpp:26-33`) -- the start lands on the
    /// node path and the source is the moved start.
    #[test]
    fn the_ctor_snaps_the_start_onto_the_path() {
        let graph = two_node_graph();
        // Draw 0 -> node a; adjacency 0 -> the a->b edge; fraction
        // 1073741823/u32::MAX = 0.25 -> the start lands a quarter along.
        let mut rng = FixedRng::new(&[0, 0, 1073741823]);
        let mut speed = 6.0f32;
        let mut start = Vector3::new(200.0, 0.0, 0.0);

        let traverser =
            ZigzagGraphTraverser::new(Rc::clone(&graph), &mut speed, &mut start, 10.0, &mut rng);

        // The snap moved the start onto the a->b path at the drawn
        // fraction.
        assert_eq!(start, Vector3::new(25.0, 0.0, 0.0));
        assert_eq!(traverser.source_pos, start);
        assert_eq!(traverser.core.destination_node, 1);
    }

    /// Acceptance: the corridor math (`zigzag_patrol_graph.cpp:42-80`)
    /// -- the poi projection on the source-destination line, the
    /// snap-to-destination inside twice the corridor, and the
    /// across-line offset outside it.
    #[test]
    fn set_zigzag_pos_projects_and_offsets() {
        let graph = two_node_graph();
        let mut rng = FixedRng::new(&[0]);
        let mut speed = 6.0f32;
        let mut start = Vector3::ZERO;

        let mut traverser =
            ZigzagGraphTraverser::new(Rc::clone(&graph), &mut speed, &mut start, 10.0, &mut rng);

        // Hand-set the corridor endpoints: a straight x-axis source
        // to destination.
        traverser.source_pos = Vector3::ZERO;
        traverser.core.destination_pos = Vector3::new(100.0, 0.0, 0.0);

        // Off the line at (25, 0, 15): poi projects to (25, 0, 0),
        // 75 away from the destination -- outside twice the corridor,
        // so the draw makes the offset. across is (0, 0, -1); a fresh
        // draw stream [0, u32::MAX] gives alongAmt 0 and acrossAmt 10.
        let mut draw_rng = FixedRng::new(&[0, u32::MAX]);
        traverser.set_zigzag_pos(&Vector3::new(25.0, 0.0, 15.0), &mut draw_rng);
        assert_eq!(traverser.zigzag_pos, Vector3::new(25.0, 0.0, -10.0));

        // Near the destination at (98, 0, 0): poi (98, 0, 0) is 2
        // away -- inside twice the corridor, so the destination wins
        // (no draws consumed).
        traverser.set_zigzag_pos(&Vector3::new(98.0, 0.0, 0.0), &mut draw_rng);
        assert_eq!(traverser.zigzag_pos, Vector3::new(100.0, 0.0, 0.0));
    }

    /// Acceptance: the vertical-line branch -- `almostEqual` on the x
    /// coordinates (`zigzag_patrol_graph.cpp:56-62`) takes the guard
    /// poi `(a.x, currPos.y, p.z)`.
    #[test]
    fn set_zigzag_pos_takes_the_vertical_line_branch() {
        let graph = two_node_graph();
        let mut rng = FixedRng::new(&[0]);
        let mut speed = 6.0f32;
        let mut start = Vector3::ZERO;

        let mut traverser =
            ZigzagGraphTraverser::new(Rc::clone(&graph), &mut speed, &mut start, 10.0, &mut rng);
        traverser.source_pos = Vector3::new(40.0, 0.0, 0.0);
        traverser.core.destination_pos = Vector3::new(40.0, 0.0, 90.0);

        // b.x == a.x: the poi sits at (40, currPos.y, currPos.z) --
        // 90 from the destination, outside the corridor; draws are 0
        // so the zigzag point is the poi itself (across is zero, the
        // normalise guard holds it at zero).
        traverser.set_zigzag_pos(&Vector3::new(10.0, 0.0, 20.0), &mut rng);
        assert_eq!(traverser.zigzag_pos, Vector3::new(40.0, 0.0, 20.0));
    }

    /// Acceptance: the step recurrence
    /// (`zigzag_patrol_graph.cpp:82-139`) -- mid-corridor walking
    /// advances the exact fraction and yaws at the zigzag point;
    /// approaching the node draws a stay; the stay's expiry retargets
    /// source and destination together.
    #[test]
    fn the_recurrence_walks_stays_and_retargets() {
        let graph = two_node_graph();
        let mut rng = FixedRng::new(&[0, 1073741823]);
        let mut speed = 6.0f32;
        let mut start = Vector3::new(200.0, 0.0, 0.0);

        let mut traverser =
            ZigzagGraphTraverser::new(Rc::clone(&graph), &mut speed, &mut start, 10.0, &mut rng);

        // Hand-set a mid-walk state: node a owns the destination
        // record; the walk target sits off-axis.
        traverser.source_pos = Vector3::ZERO;
        traverser.core.destination_node = 0;
        traverser.core.destination_pos = Vector3::new(100.0, 0.0, 0.0);
        traverser.zigzag_pos = Vector3::new(100.0, 0.0, 20.0);
        traverser.core.stay_time = -1.0;

        let mut pos = Vector3::ZERO;
        let mut dir = Direction3D::new(0.0);
        let mut walk_rng = FixedRng::new(&[0]);

        // In transit: no branch fires; the move advances the exact
        // fraction of the zigzag vector (length sqrt(100^2+20^2)) at
        // distance 6*0.5 = 3.
        let zz_vec = Vector3::new(100.0, 0.0, 20.0);
        let scale = 3.0 / zz_vec.length();
        assert!(traverser.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut walk_rng));
        assert_eq!(pos, zz_vec * scale);
        assert_eq!(dir.yaw, 100.0f32.atan2(20.0));

        // Approaching the node: destinationPos within distance draws
        // a stay (minStay/maxStay default to 0 -> 0) and re-aims at
        // the node's destination draw -- the draw circles the *node*
        // position (a sits at the origin; radius 0 lands there), not
        // the hand-set destination. The unguarded move then advances
        // along the new zz vector.
        traverser.core.stay_time = -1.0;
        traverser.core.destination_node = 0;
        traverser.core.destination_pos = Vector3::new(100.0, 0.0, 0.0);
        traverser.zigzag_pos = Vector3::new(100.0, 0.0, 0.0);
        pos = Vector3::new(99.0, 0.0, 0.0);
        assert!(traverser.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut walk_rng));
        assert_eq!(traverser.core.stay_time, 0.0);
        assert_eq!(traverser.zigzag_pos, Vector3::ZERO);

        // Stay expiry: hand-seed 0.5 so the strict stay branch runs;
        // the expiry retargets source to the node, hops to b, redraws
        // the destination and the zigzag point (the poi lands inside
        // twice the corridor, so the destination wins), then moves.
        // Unlike the patrol traverser the expired stay is left at
        // zero -- no in-transit reset -- so the next tick falls
        // through to the approach branch.
        traverser.core.stay_time = 0.5;
        assert!(traverser.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut walk_rng));
        assert_eq!(traverser.core.destination_node, 1);
        assert_eq!(traverser.source_pos, Vector3::ZERO);
        assert_eq!(traverser.core.stay_time, 0.0);
        assert_eq!(traverser.zigzag_pos, Vector3::new(100.0, 0.0, 0.0));

        // Hanging around but arrived at the zzp: a positive stay
        // ticks down without expiring and the zzp redraws from the
        // destination node's draw (b sits at (100, 0, 0); radius 0
        // lands there).
        traverser.core.stay_time = 1.0;
        pos = Vector3::new(102.0, 0.0, 0.0);
        traverser.zigzag_pos = Vector3::new(100.0, 0.0, 0.0);
        assert!(traverser.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut walk_rng));
        assert_eq!(traverser.core.stay_time, 0.5);
        assert_eq!(traverser.zigzag_pos, Vector3::new(100.0, 0.0, 0.0));
    }

    /// Acceptance: arrived at the zzp in transit -- no stay, the node
    /// still far, the zz vector inside the step: the zigzag point
    /// recomputes (`zigzag_patrol_graph.cpp:120-123`).
    #[test]
    fn the_zzp_recomputes_when_reached_in_transit() {
        let graph = two_node_graph();
        let mut rng = FixedRng::new(&[0]);
        let mut speed = 6.0f32;
        let mut start = Vector3::ZERO;

        let mut traverser =
            ZigzagGraphTraverser::new(Rc::clone(&graph), &mut speed, &mut start, 10.0, &mut rng);

        // Hand-set: in transit toward b, one step from the zzp.
        traverser.source_pos = Vector3::ZERO;
        traverser.core.destination_node = 1;
        traverser.core.destination_pos = Vector3::new(100.0, 0.0, 0.0);
        traverser.core.stay_time = -1.0;
        traverser.zigzag_pos = Vector3::new(96.0, 0.0, 0.0);

        let mut pos = Vector3::new(94.0, 0.0, 0.0);
        let mut dir = Direction3D::new(0.0);
        let mut walk_rng = FixedRng::new(&[0]);

        // destVec 6 is not < distance 3; zzVec 2 is: setZigzagPos
        // runs, and the poi (94, 0, 0) sits inside twice the corridor
        // so the destination wins.
        assert!(traverser.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut walk_rng));
        assert_eq!(traverser.zigzag_pos, Vector3::new(100.0, 0.0, 0.0));
        assert_eq!(traverser.core.stay_time, -1.0);
    }

    /// Acceptance: the factory parse
    /// (`zigzag_patrol_graph.cpp:170-202`) -- first-comma split, the
    /// width through `c_atof`, non-positive falls back to the 50
    /// default, and the load failure carries the zigzag text (no
    /// quotes) `Unable to load %s`.
    #[test]
    fn the_factory_parses_width_and_falls_back() {
        let dir = std::env::temp_dir().join(format!("boundless_zigzag_{}_0", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            format!("{}/graphs.zigzag", dir.display()),
            "<root><nodes>\
             <node><name>a</name><pos>0 0 0</pos><radius>0</radius>\
             <edges><edge>b</edge></edges></node>\
             <node><name>b</name><pos>100 0 0</pos><radius>0</radius>\
             <edges><edge>a</edge></edges></node></nodes></root>",
        )
        .unwrap();
        let resource = BwResource::init_paths(&[dir.to_string_lossy().as_ref()]);

        let mut factory = ZigzagPatrolFactory::new();
        let _ = ZigzagPatrolFactory::default();
        let mut speed = 6.0f32;
        let mut position = Vector3::ZERO;
        let mut rng = FixedRng::new(&[0, 1073741823]);

        // The missing graph: the zigzag IOError text (no quotes, no
        // newline).
        let err = factory
            .create(
                &resource,
                "nope.zigzag",
                &mut speed,
                &mut position,
                &mut rng,
            )
            .err()
            .expect("expected the graph-load error");
        assert_eq!(err.kind, MovementErrorKind::IoError);
        assert_eq!(err.message, "Unable to load nope.zigzag");

        // A width parses through the C prefix rules and lands on the
        // controller.
        let mut controller = factory
            .create(
                &resource,
                "graphs.zigzag,12.5",
                &mut speed,
                &mut position,
                &mut rng,
            )
            .unwrap()
            .unwrap();
        let mut dir3 = Direction3D::new(0.0);
        assert!(controller.next_step(&mut speed, 0.5, &mut position, &mut dir3, &mut rng));

        // Junk width (atof 0) and the bare name fall back to the
        // default 50 corridor.
        let mut controller = factory
            .create(
                &resource,
                "graphs.zigzag,wat",
                &mut speed,
                &mut position,
                &mut rng,
            )
            .unwrap()
            .unwrap();
        let mut dir3 = Direction3D::new(0.0);
        assert!(controller.next_step(&mut speed, 0.5, &mut position, &mut dir3, &mut rng));

        let mut controller = factory
            .create(
                &resource,
                "graphs.zigzag",
                &mut speed,
                &mut position,
                &mut rng,
            )
            .unwrap()
            .unwrap();
        let mut dir3 = Direction3D::new(0.0);
        assert!(controller.next_step(&mut speed, 0.5, &mut position, &mut dir3, &mut rng));

        // The cache lives under the unit scales.
        assert!(
            factory
                .graphs
                .graphs
                .contains_key("graphs.zigzag,1.000000,1.000000")
        );
    }
}
