//! The Patrol graph face (`server/tools/bots/patrol_graph.{hpp,cpp}`):
//! the node graph the patrol controller walks, the graph cache, the
//! traverser and the "Patrol" factory.
//!
//! Reference quirks preserved:
//! - `Graphs::getGraph` caches under a decorated name
//!   `"%s,%f,%f"` (six decimals), purging the section cache before the
//!   reload (`patrol_graph.cpp:324-359`);
//! - an empty adjacency makes `nextNode` answer the node itself
//!   (`patrol_graph.cpp:120-130`);
//! - the traverser's negative stay time means *in transit*;
//!   `nextStep` draws a stay when it arrives (`patrol_graph.cpp:401-444`);
//! - a zero-length direction with zero distance walks into `0/0` --
//!   the reference's divide is unguarded and the port keeps it;
//! - a `Patrol` data argument that is not `random`/`snap`/`scalePos=`/
//!   `scaleSpeed=` raises ValueError *and* the reference still builds a
//!   traverser, which the caller then rejects through
//!   `PyErr_Occurred`; the port folds the failure into the return
//!   (the only visible difference: the position-mutation the reference's
//!   doomed traverser performed does not happen).

use std::collections::BTreeMap;
use std::rc::Rc;

use foundation::math::Vector3;
use resources::bwresource::BwResource;
use resources::datasection::DataSection;

use crate::movement::{
    Direction3D, MovementController, MovementError, MovementErrorKind, MovementFactory, Rng, c_atof,
};

/// `Node::Properties` (`patrol_graph.hpp:26-53`): all zeros at rest.
#[derive(Clone, Debug, Default)]
pub struct Properties {
    min_stay: f32,
    max_stay: f32,
    radius: f32,
    min_speed: f32,
    max_speed: f32,
}

/// `DataSection::readFloat`'s shape: the child's float or the default.
fn read_float(section: &DataSection, tag: &str, default: f32) -> f32 {
    match section.find_child(tag) {
        Some(child) => child.as_float(default),
        None => default,
    }
}

impl Properties {
    /// `Node::Properties::init` (`patrol_graph.cpp:189-198`): each
    /// property reads its tag with the current value as default; a
    /// null section is a no-op.
    pub fn init(&mut self, section: Option<&DataSection>) {
        let Some(section) = section else {
            return;
        };
        self.min_stay = read_float(section, "minStay", self.min_stay);
        self.max_stay = read_float(section, "maxStay", self.max_stay);
        self.radius = read_float(section, "radius", self.radius);
        self.min_speed = read_float(section, "minSpeed", self.min_speed);
        self.max_speed = read_float(section, "maxSpeed", self.max_speed);
    }
}

/// `Patrol::Node` (`patrol_graph.hpp:23-88`).
#[derive(Clone, Debug, Default)]
pub struct Node {
    to_nodes: Vec<i32>,
    position: Vector3,
    id: i32,
    name: String,
    props: Properties,
}

/// `Node::NodeMap` (`patrol_graph.hpp:57`).
pub type NodeMap = BTreeMap<String, i32>;

impl Node {
    /// `Node::init1` (`patrol_graph.cpp:34-60`): identity, position,
    /// properties over the defaults; an empty name becomes the index
    /// text.
    pub fn init1(
        &mut self,
        index: i32,
        section: Option<&DataSection>,
        default_props: &Properties,
    ) -> bool {
        self.props = default_props.clone();

        let Some(section) = section else {
            return false;
        };

        self.id = index;
        self.position = section.read_vector3("pos", Vector3::ZERO);
        self.name = section.read_string("name", "");

        self.props.init(Some(section));

        if self.name.is_empty() {
            self.name = format!("{index}");
        }

        true
    }

    /// `Node::init2` (`patrol_graph.cpp:62-94`): resolve every edge
    /// name against the node map; an unknown edge fails the node.
    pub fn init2(&mut self, map: &NodeMap, section: Option<&DataSection>) -> bool {
        let Some(section) = section else {
            return false;
        };
        let p_edges = section.find_child("edges");

        let num_edges = match p_edges {
            Some(edges) => edges.count_children(),
            None => {
                eprintln!("Graph::init2: No edges section");
                0
            }
        };

        self.to_nodes = vec![0; num_edges];

        for i in 0..num_edges {
            let dst_name = p_edges
                .and_then(|edges| edges.open_child(i))
                .map(|child| child.as_string().to_string())
                .unwrap_or_default();

            let Some(&target) = map.get(&dst_name) else {
                eprintln!(
                    "Graph::init2: Could not find {} for {}",
                    dst_name, self.name
                );
                return false;
            };
            self.to_nodes[i] = target;
        }

        true
    }

    /// `Node::flatDistSqrTo` (`patrol_graph.cpp:100-105`).
    pub fn flat_dist_sqr_to(&self, pos: &Vector3) -> f32 {
        let x = self.position.x - pos.x;
        let z = self.position.z - pos.z;
        x * x + z * z
    }

    /// `Node::flatDistTo` (`patrol_graph.cpp:111-114`).
    pub fn flat_dist_to(&self, pos: &Vector3) -> f32 {
        self.flat_dist_sqr_to(pos).sqrt()
    }

    /// `Node::nextNode` (`patrol_graph.cpp:120-130`): a random
    /// adjacency, or the node itself with none.
    pub fn next_node(&self, rng: &mut dyn Rng) -> i32 {
        if !self.to_nodes.is_empty() {
            self.to_nodes[(rng.next_u32() as usize) % self.to_nodes.len()]
        } else {
            self.id
        }
    }

    /// `Node::randomisePosition` (`patrol_graph.cpp:137-150`): put
    /// `pos` on the path to a random adjacency and answer which one.
    pub fn randomise_position(&self, pos: &mut Vector3, graph: &Graph, rng: &mut dyn Rng) -> i32 {
        if self.to_nodes.is_empty() {
            *pos = self.position;
            return self.id;
        }

        let next = self.to_nodes[(rng.next_u32() as usize) % self.to_nodes.len()];
        *pos = self.position
            + (graph.node(next).position - self.position)
                * (rng.next_u32() as f32 / crate::movement::RAND_MAX_U32 as f32);
        next
    }

    /// `Node::generateDestination` (`patrol_graph.cpp:156-163`): a
    /// random point in the node's radius square at the node's height.
    pub fn generate_destination(&self, rng: &mut dyn Rng) -> Vector3 {
        let radius = self.props.radius;
        Vector3::new(
            rng.rand_in_range(self.position.x - radius, self.position.x + radius),
            self.position.y,
            rng.rand_in_range(self.position.z - radius, self.position.z + radius),
        )
    }

    /// `Node::generateStayTime` (`patrol_graph.cpp:169-172`).
    pub fn generate_stay_time(&self, rng: &mut dyn Rng) -> f32 {
        rng.rand_in_range(self.props.min_stay, self.props.max_stay)
    }

    /// `Node::generateSpeed` (`patrol_graph.cpp:178-183`): the node's
    /// draw, or the incoming speed with no speed properties.
    pub fn generate_speed(&self, ospeed: f32, rng: &mut dyn Rng) -> f32 {
        if self.props.min_speed <= 0.0 || self.props.max_speed <= 0.0 {
            return ospeed;
        }
        rng.rand_in_range(self.props.min_speed, self.props.max_speed)
    }

    /// `Node::name`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// `Node::position`.
    pub fn position(&self) -> &Vector3 {
        &self.position
    }
}

/// `Patrol::Graph` (`patrol_graph.hpp:94-108`).
#[derive(Clone, Debug, Default)]
pub struct Graph {
    nodes: Vec<Node>,
}

impl Graph {
    /// `Graph::init` (`patrol_graph.cpp:216-269`): the defaults, the
    /// node pass (scaled, duplicate-name checked), then the edge
    /// pass. Errors carry the reference's ERROR_MSG text.
    pub fn init(
        &mut self,
        section: &DataSection,
        scale_pos: f32,
        scale_speed: f32,
    ) -> Result<(), String> {
        let Some(p_nodes) = section.find_child("nodes") else {
            return Err("Graph::init: No nodes section".to_string());
        };

        let mut node_defaults = Properties::default();
        node_defaults.init(section.find_child("nodeDefaults"));

        let num_nodes = p_nodes.count_children();
        self.nodes = vec![Node::default(); num_nodes];

        let mut node_map = NodeMap::new();

        for (i, node) in self.nodes.iter_mut().enumerate() {
            let p_node = p_nodes.open_child(i);
            if !node.init1(i as i32, p_node, &node_defaults) {
                return Err(format!("Graph::init: Failed to initialise node {i}"));
            }
            node.position = node.position * scale_pos;
            node.props.radius *= scale_pos;
            node.props.min_speed *= scale_speed;
            node.props.max_speed *= scale_speed;

            let name = node.name().to_string();
            if node_map.insert(name.clone(), i as i32).is_some() {
                return Err(format!("Graph::init: Node {name} is a duplicate"));
            }
        }

        for (i, node) in self.nodes.iter_mut().enumerate() {
            let p_node = p_nodes.open_child(i);
            if !node.init2(&node_map, p_node) {
                return Err(format!(
                    "Graph::init: Failed to initialise edges for node {i}"
                ));
            }
        }

        Ok(())
    }

    /// `Graph::findClosestNode` (`patrol_graph.cpp:275-297`): flat
    /// squared distances, -1 with no nodes.
    pub fn find_closest_node(&self, position: &Vector3) -> i32 {
        if self.nodes.is_empty() {
            return -1;
        }

        let mut best_dist = self.nodes[0].flat_dist_sqr_to(position);
        let mut best_index = 0usize;

        for (i, node) in self.nodes.iter().enumerate().skip(1) {
            let dist = node.flat_dist_sqr_to(position);
            if dist < best_dist {
                best_dist = dist;
                best_index = i;
            }
        }

        best_index as i32
    }

    /// `Graph::node` (`patrol_graph.cpp:303-306`): unchecked in the
    /// reference; the callers keep the index in range.
    pub fn node(&self, index: i32) -> &Node {
        &self.nodes[index as usize]
    }

    /// `Graph::size`.
    pub fn size(&self) -> usize {
        self.nodes.len()
    }
}

/// `Patrol::Graphs` (`patrol_graph.hpp:114-124`): the graph cache.
#[derive(Debug, Default)]
pub struct Graphs {
    pub(crate) graphs: BTreeMap<String, Rc<Graph>>,
}

impl Graphs {
    /// `Graphs::new`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `Graphs::getGraph` (`patrol_graph.cpp:324-359`): the decorated
    /// cache key, the purge-then-open reload, the erase on a failed
    /// init.
    pub fn get_graph(
        &mut self,
        resource: &BwResource,
        graph_name: &str,
        scale_pos: f32,
        scale_speed: f32,
    ) -> Option<Rc<Graph>> {
        if graph_name.len() > 1024 - 64 {
            return None;
        }

        // bw_snprintf "%s,%f,%f": %f is six decimals.
        let decorated_name = format!("{graph_name},{scale_pos:.6},{scale_speed:.6}");

        if let Some(graph) = self.graphs.get(&decorated_name) {
            return Some(Rc::clone(graph));
        }

        resource.purge(graph_name, false);
        let p_section = resource.open_section(graph_name, false)?;

        let mut graph = Graph::default();
        if graph.init(&p_section, scale_pos, scale_speed).is_err() {
            return None;
        }
        let graph = Rc::new(graph);
        self.graphs.insert(decorated_name, Rc::clone(&graph));
        Some(graph)
    }
}

/// The traverser state shared by the patrol and zigzag walkers
/// (`GraphTraverser`'s protected fields, `patrol_graph.hpp:130-143`).
pub(crate) struct GraphTraverserCore {
    pub(crate) destination_node: i32,
    pub(crate) destination_pos: Vector3,
    pub(crate) stay_time: f32,
    pub(crate) graph: Rc<Graph>,
}

impl GraphTraverserCore {
    /// `GraphTraverser::GraphTraverser`
    /// (`patrol_graph.cpp:369-395`): head for a random node or the
    /// closest one; `snap` puts the start on a path and re-targets;
    /// the node re-draws the speed.
    pub(crate) fn new(
        graph: Rc<Graph>,
        speed: &mut f32,
        start_position: &mut Vector3,
        randomise: bool,
        snap: bool,
        rng: &mut dyn Rng,
    ) -> Self {
        // head for either the closest node or any old node
        let mut destination_node = if randomise {
            (rng.next_u32() as usize % graph.size()) as i32
        } else {
            graph.find_closest_node(start_position)
        };
        let stay_time = -1.0f32;

        let mut p_node = graph.node(destination_node);
        // start off either where we were or on the path to a
        // neighbouring node
        if snap {
            // startPosition is modified:
            destination_node = p_node.randomise_position(start_position, &graph, rng);
            p_node = graph.node(destination_node);
        }

        let destination_pos = p_node.generate_destination(rng);
        *speed = p_node.generate_speed(*speed, rng);

        Self {
            destination_node,
            destination_pos,
            stay_time,
            graph,
        }
    }

    /// `GraphTraverser::nextStep` (`patrol_graph.cpp:401-444`).
    pub(crate) fn next_step(
        &mut self,
        speed: &mut f32,
        d_time: f32,
        pos: &mut Vector3,
        dir: &mut Direction3D,
        rng: &mut dyn Rng,
    ) -> bool {
        let mut p_node = self.graph.node(self.destination_node);

        if self.stay_time >= 0.0 {
            self.stay_time -= d_time;

            if self.stay_time <= 0.0 {
                self.destination_node = p_node.next_node(rng);
                p_node = self.graph.node(self.destination_node);
                self.destination_pos = p_node.generate_destination(rng);
                self.stay_time = -1.0;
                *speed = p_node.generate_speed(*speed, rng);
            }
        }

        let distance = *speed * d_time;

        let dir_vec = self.destination_pos - *pos;
        let length = dir_vec.length();

        // TODO: When is near enough
        if length < distance {
            // Note: Negative stay time means we are in transit.
            if self.stay_time < 0.0 {
                self.stay_time = p_node.generate_stay_time(rng);
            }

            self.destination_pos = p_node.generate_destination(rng);
        } else {
            *pos = *pos + dir_vec * (distance / length);
            dir.yaw = dir_vec.yaw();
        }

        true
    }
}

/// `Patrol::GraphTraverser` (`patrol_graph.hpp:130-143`).
pub struct GraphTraverser(GraphTraverserCore);

impl MovementController for GraphTraverser {
    fn next_step(
        &mut self,
        speed: &mut f32,
        d_time: f32,
        pos: &mut Vector3,
        dir: &mut Direction3D,
        rng: &mut dyn Rng,
    ) -> bool {
        self.0.next_step(speed, d_time, pos, dir, rng)
    }
}

/// `PatrolFactory` (`patrol_graph.cpp:456-520`): the "Patrol"
/// factory; its graph cache lives as long as the factory.
pub struct PatrolFactory {
    graphs: Graphs,
}

impl PatrolFactory {
    /// The constructor.
    pub fn new() -> Self {
        Self {
            graphs: Graphs::new(),
        }
    }
}

impl Default for PatrolFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl MovementFactory for PatrolFactory {
    /// `PatrolFactory::create` (`patrol_graph.cpp:467-514`): the
    /// comma-split argument walk, then the graph, then the traverser.
    fn create(
        &mut self,
        resource: &BwResource,
        data: &str,
        speed: &mut f32,
        position: &mut Vector3,
        rng: &mut dyn Rng,
    ) -> Result<Option<Box<dyn MovementController>>, MovementError> {
        let mut graph_name = String::new();
        let mut randomise = false;
        let mut snap = false;
        let mut scale_pos = 1.0f32;
        let mut scale_speed = 1.0f32;
        let mut unknown: Option<MovementError> = None;

        let mut argbeg = 0usize;
        let mut argnum = 0i32;
        loop {
            let argend = match data[argbeg..].find(',') {
                Some(offset) => argbeg + offset,
                None => data.len(),
            };
            let argstr = &data[argbeg..argend];

            if argnum == 0 {
                graph_name = argstr.to_string();
            } else if argstr == "random" {
                randomise = true;
            } else if argstr == "snap" {
                snap = true;
            } else if let Some(rest) = argstr.strip_prefix("scalePos=") {
                scale_pos = c_atof(rest);
            } else if let Some(rest) = argstr.strip_prefix("scaleSpeed=") {
                scale_speed = c_atof(rest);
            } else {
                unknown = Some(MovementError::new(
                    MovementErrorKind::ValueError,
                    format!("PatrolFactory::create: Unknown argument '{argstr}'\n"),
                ));
            }
            argbeg = argend + 1;
            argnum += 1;
            if argbeg >= data.len() {
                break;
            }
        }

        if let Some(err) = unknown {
            // The reference keeps going with PyErr set: it still loads
            // the graph and builds the traverser, so the snap's
            // position/speed mutations happen, and only then does the
            // caller reject the controller through PyErr_Occurred. The
            // port keeps the mutations and folds the failure into the
            // return. A failing graph load replaces the error, exactly
            // like PyErr_Format overwrites it.
            let Some(p_graph) =
                self.graphs
                    .get_graph(resource, &graph_name, scale_pos, scale_speed)
            else {
                return Err(MovementError::new(
                    MovementErrorKind::IoError,
                    format!("Unable to load graph '{graph_name}'"),
                ));
            };
            let _ = GraphTraverserCore::new(p_graph, speed, position, randomise, snap, rng);
            return Err(err);
        }

        let Some(p_graph) = self
            .graphs
            .get_graph(resource, &graph_name, scale_pos, scale_speed)
        else {
            return Err(MovementError::new(
                MovementErrorKind::IoError,
                format!("Unable to load graph '{graph_name}'"),
            ));
        };

        Ok(Some(Box::new(GraphTraverser(GraphTraverserCore::new(
            p_graph, speed, position, randomise, snap, rng,
        )))))
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

    fn graph_xml(body: &str) -> DataSection {
        let xml = format!("<root><nodes>{body}</nodes></root>");
        resources::datasection::parse("root", xml.as_bytes()).unwrap()
    }

    /// Acceptance: the graph load (`patrol_graph.cpp:216-269`) --
    /// positions and names read, defaults applied, scaling applied,
    /// the empty name becomes the index text.
    #[test]
    fn the_graph_loads_nodes_with_defaults_and_scale() {
        let xml = graph_xml(
            "<node><name>a</name><pos>10 0 20</pos><radius>5</radius>\
             <edges><edge>b</edge></edges></node>\
             <node><name>b</name><pos>30 0 40</pos><edges><edge>a</edge></edges></node>\
             <node><pos>0 0 0</pos></node>",
        );
        let mut graph = Graph::default();
        graph.init(&xml, 2.0, 3.0).unwrap();
        assert_eq!(graph.size(), 3);
        assert_eq!(graph.node(0).name(), "a");
        assert_eq!(graph.node(0).position(), &Vector3::new(20.0, 0.0, 40.0));
        assert_eq!(graph.node(0).props.radius, 10.0);
        // The unnamed node gets its index text.
        assert_eq!(graph.node(2).name(), "2");
        // Edges resolved by name.
        assert_eq!(graph.node(0).to_nodes, vec![1]);
        assert_eq!(graph.node(1).to_nodes, vec![0]);
        // A missing edges section just warns.
        assert_eq!(graph.node(2).to_nodes, Vec::<i32>::new());
    }

    /// Acceptance: a missing nodes section fails with the reference's
    /// message (`patrol_graph.cpp:220-224`), duplicate names fail
    /// (`:248-253`), unknown edges fail (`patrol_graph.cpp:83-88`).
    #[test]
    fn the_graph_load_rejects_bad_graphs() {
        let no_nodes = resources::datasection::parse("root", b"<root/>").unwrap();
        let mut graph = Graph::default();
        assert_eq!(
            graph.init(&no_nodes, 1.0, 1.0),
            Err("Graph::init: No nodes section".to_string())
        );

        let dup = graph_xml(
            "<node><name>a</name><pos>0 0 0</pos></node>\
             <node><name>a</name><pos>1 0 1</pos></node>",
        );
        assert_eq!(
            graph.init(&dup, 1.0, 1.0),
            Err("Graph::init: Node a is a duplicate".to_string())
        );

        let dangling = graph_xml(
            "<node><name>a</name><pos>0 0 0</pos>\
             <edges><edge>ghost</edge></edges></node>",
        );
        assert_eq!(
            graph.init(&dangling, 1.0, 1.0),
            Err("Graph::init: Failed to initialise edges for node 0".to_string())
        );
    }

    /// Acceptance: `findClosestNode` (`patrol_graph.cpp:275-297`) and
    /// the empty-graph -1.
    #[test]
    fn find_closest_node_answers_the_flat_sqr_winner() {
        let xml = graph_xml(
            "<node><name>a</name><pos>10 0 10</pos></node>\
             <node><name>b</name><pos>90 0 90</pos></node>",
        );
        let mut graph = Graph::default();
        graph.init(&xml, 1.0, 1.0).unwrap();
        assert_eq!(graph.find_closest_node(&Vector3::new(0.0, 0.0, 0.0)), 0);
        assert_eq!(graph.find_closest_node(&Vector3::new(80.0, 0.0, 80.0)), 1);
        assert_eq!(Graph::default().find_closest_node(&Vector3::ZERO), -1);
    }

    /// Acceptance: `nextNode` with no edges answers the node itself
    /// (`patrol_graph.cpp:120-130`), and `generateSpeed` keeps the
    /// incoming speed when no speed properties exist
    /// (`patrol_graph.cpp:178-183`).
    #[test]
    fn node_draws_respect_their_guards() {
        let mut rng = FixedRng::new(&[0]);
        let node = Node::default();
        assert_eq!(node.next_node(&mut rng), node.id);
        assert_eq!(node.generate_speed(6.5, &mut rng), 6.5);
    }

    /// Acceptance: the node guards -- `init1`/`init2` fail on a
    /// missing section (`patrol_graph.cpp:36-39,72-75`),
    /// `randomisePosition` lands an edge-less node on itself
    /// (`patrol_graph.cpp:133-140`), and `generateSpeed` draws inside
    /// real bounds (`patrol_graph.cpp:178-183`).
    #[test]
    fn node_guards_answer_their_fallbacks() {
        let mut node = Node::default();
        assert!(!node.init1(0, None, &Properties::default()));
        let map = NodeMap::new();
        assert!(!node.init2(&map, None));

        // An edge-less node: the position is kept, the node itself
        // is the answer -- no draws.
        node.position = Vector3::new(7.0, 0.0, 9.0);
        let graph = Graph::default();
        let mut rng = FixedRng::new(&[0]);
        let mut pos = Vector3::ZERO;
        let id = node.randomise_position(&mut pos, &graph, &mut rng);
        assert_eq!(id, node.id);
        assert_eq!(pos, Vector3::new(7.0, 0.0, 9.0));

        // Real speed bounds draw inside them: 2 + 0.25 * 3.
        node.props.min_speed = 2.0;
        node.props.max_speed = 5.0;
        let mut rng = FixedRng::new(&[1073741823]);
        assert_eq!(node.generate_speed(6.0, &mut rng), 2.75);

        // The flat distance is the planar hypotenuse.
        assert_eq!(node.flat_dist_to(&Vector3::new(7.0, 40.0, 12.0)), 3.0);
    }

    /// Acceptance: `Graphs::getGraph` guards -- a name past
    /// `sizeof(dnbuf) - 64` is refused (`patrol_graph.cpp:331-334`)
    /// and a section whose graph fails init erases out
    /// (`patrol_graph.cpp:344-348`).
    #[test]
    fn get_graph_rejects_long_names_and_broken_files() {
        let dir = std::env::temp_dir().join(format!("boundless_patrol_{}_1", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(format!("{}/broken.patrol", dir.display()), "<root/>").unwrap();
        let resource = BwResource::init_paths(&[dir.to_string_lossy().as_ref()]);

        let mut graphs = Graphs::new();
        let long = "x".repeat(1024 - 64 + 1);
        assert!(graphs.get_graph(&resource, &long, 1.0, 1.0).is_none());
        assert!(
            graphs
                .get_graph(&resource, "broken.patrol", 1.0, 1.0)
                .is_none()
        );
        assert!(graphs.graphs.is_empty(), "nothing cached");
    }

    /// Acceptance: the traverser constructor
    /// (`patrol_graph.cpp:369-395`) -- snap puts the start on a path
    /// and re-targets the landing node; the speed re-draw keeps the
    /// incoming speed with no node speeds.
    #[test]
    fn the_traverser_ctor_snaps_onto_a_path() {
        let xml = graph_xml(
            "<node><name>a</name><pos>0 0 0</pos><radius>0</radius>\
             <edges><edge>b</edge></edges></node>\
             <node><name>b</name><pos>100 0 0</pos><radius>0</radius>\
             <edges><edge>a</edge></edges></node>",
        );
        let mut graph = Graph::default();
        graph.init(&xml, 1.0, 1.0).unwrap();
        let graph = Rc::new(graph);

        // Draw 0 -> adjacency index 0 -> node b; fraction 0 lands the
        // start exactly on node a -- randomisePosition always writes
        // the position, even at fraction zero.
        let mut rng = FixedRng::new(&[0, 0]);
        let mut speed = 6.0f32;
        let mut start = Vector3::new(50.0, 0.0, 0.0);
        let core = GraphTraverserCore::new(
            Rc::clone(&graph),
            &mut speed,
            &mut start,
            false,
            true,
            &mut rng,
        );
        assert_eq!(core.destination_node, 1);
        assert_eq!(start, Vector3::ZERO);
        // generateDestination with radius 0 lands on the node.
        assert_eq!(core.destination_pos, Vector3::new(100.0, 0.0, 0.0));
    }

    /// Acceptance: the traverser step recurrence
    /// (`patrol_graph.cpp:401-444`) -- walk the exact fraction, draw a
    /// stay on arrival, re-target when the stay expires.
    #[test]
    fn the_traverser_recurrence_walks_stays_and_retargets() {
        let xml = graph_xml(
            "<node><name>a</name><pos>0 0 0</pos><radius>0</radius>\
             <minStay>1</minStay><maxStay>1</maxStay>\
             <edges><edge>b</edge></edges></node>\
             <node><name>b</name><pos>40 0 0</pos><radius>0</radius>\
             <edges><edge>a</edge></edges></node>",
        );
        let mut graph = Graph::default();
        graph.init(&xml, 1.0, 1.0).unwrap();
        let graph = Rc::new(graph);

        // Start at a (closest), no snap: destination node a.
        let mut rng = FixedRng::new(&[0, 0, 0, 0]);
        let mut speed = 10.0f32;
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut core = GraphTraverserCore::new(
            Rc::clone(&graph),
            &mut speed,
            &mut pos,
            false,
            false,
            &mut rng,
        );
        assert_eq!(core.destination_node, 0);
        assert_eq!(core.destination_pos, Vector3::ZERO);

        // Tick 1: destination length 0 < distance 5 -> in-transit stay
        // draw (minStay=maxStay=1) and a same-spot re-destination.
        let mut dir = Direction3D::new(0.0);
        assert!(core.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut rng));
        assert_eq!(core.stay_time, 1.0);

        // Tick 2: stay runs down 1 - 0.5 = 0.5; then the distance
        // branch: pos sits on the destination (length 0 < 5) so no
        // walk, stay 0.5 is not < 0 so no redraw, destination
        // re-drawn.
        assert!(core.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut rng));
        assert_eq!(core.stay_time, 0.5);

        // Tick 3: stay expires (0.5 - 0.5 = 0 <= 0) -> next node b via
        // the edge, destination b, stay -1, speed redraw keeps 10 --
        // and the distance is computed after the retarget, so the
        // same tick walks the first fraction toward b: distance 5
        // over length 40 -> pos.x = 5.
        assert!(core.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut rng));
        assert_eq!(core.destination_node, 1);
        assert_eq!(core.stay_time, -1.0);
        assert_eq!(pos.x, 5.0);
        assert_eq!(dir.yaw, 40.0f32.atan2(0.0));

        // Tick 4: still in transit: distance 5 over length 35 -> pos.x = 10.
        assert!(core.next_step(&mut speed, 0.5, &mut pos, &mut dir, &mut rng));
        assert_eq!(pos.x, 10.0);
    }

    /// Acceptance: the "Patrol" factory parse
    /// (`patrol_graph.cpp:467-514`) -- flags and scales, the unknown
    /// argument ValueError text, the graph-load IOError text.
    #[test]
    fn the_patrol_factory_parses_its_arguments() {
        let dir = std::env::temp_dir().join(format!("boundless_patrol_{}_0", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            format!("{}/graphs.patrol", dir.display()),
            "<root><nodes>\
             <node><name>a</name><pos>0 0 0</pos><radius>2</radius>\
             <edges><edge>a</edge></edges></node></nodes></root>",
        )
        .unwrap();
        let resource = BwResource::init_paths(&[dir.to_string_lossy().as_ref()]);

        let mut factory = PatrolFactory::new();
        let _ = PatrolFactory::default();
        let mut speed = 6.0f32;
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut rng = FixedRng::new(&[0]);

        // Unknown argument first: ValueError, parse continues to the
        // end but the port folds it into the return
        // (`patrol_graph.cpp:497-500`).
        let err = factory
            .create(
                &resource,
                "graphs.patrol,random,wat",
                &mut speed,
                &mut pos,
                &mut rng,
            )
            .err()
            .expect("expected the unknown-argument error");
        assert_eq!(err.kind, MovementErrorKind::ValueError);
        assert_eq!(
            err.message,
            "PatrolFactory::create: Unknown argument 'wat'\n"
        );

        // An unknown argument over a *missing* graph: the load
        // failure's PyErr_Format replaces the pending error.
        let err = factory
            .create(&resource, "nope.patrol,wat", &mut speed, &mut pos, &mut rng)
            .err()
            .expect("expected the overwrite error");
        assert_eq!(err.kind, MovementErrorKind::IoError);
        assert_eq!(err.message, "Unable to load graph 'nope.patrol'");

        // A missing graph: IOError with the reference text.
        let err = factory
            .create(&resource, "nope.patrol", &mut speed, &mut pos, &mut rng)
            .err()
            .expect("expected the graph-load error");
        assert_eq!(err.kind, MovementErrorKind::IoError);
        assert_eq!(err.message, "Unable to load graph 'nope.patrol'");

        // The working load, then the decorated cache hit.
        let mut controller = factory
            .create(
                &resource,
                "graphs.patrol,snap,random,scalePos=2,scaleSpeed=0.5",
                &mut speed,
                &mut pos,
                &mut rng,
            )
            .unwrap()
            .unwrap();
        let mut dir3 = Direction3D::new(0.0);
        assert!(controller.next_step(&mut speed, 0.5, &mut pos, &mut dir3, &mut rng));
        assert!(
            factory
                .graphs
                .graphs
                .contains_key("graphs.patrol,2.000000,0.500000")
        );
    }
}
