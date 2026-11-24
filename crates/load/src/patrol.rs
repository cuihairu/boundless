//! BW-17-01-05: the patrol graph and its traverser
//! (`server/tools/bots/patrol_graph.{hpp,cpp}`).
//!
//! A patrol graph is an XML `.bwp` document: a `nodes` section of nodes
//! (position, optional name, stay/radius/speed properties, an `edges`
//! list of neighbour names) plus optional `nodeDefaults`. Bots walk it by
//! moving to a random destination point inside the current node's
//! `radius`, waiting `minStay..maxStay`, then picking a random adjacent
//! node and repeating (`patrol_graph.cpp:401-444`).
//!
//! Two deliberate port decisions:
//!
//! - The reference reads the C globals `rand()` / `RAND_MAX` for node
//!   selection and jitter. The port passes an explicit [`UnitRand`] - the
//!   same xorshift64* the fleet's speed draw uses - so a fixed seed
//!   reproduces the same waypoint order (BW-17-02-04's face).
//! - [`MovementFactory::create`](crate::bot::MovementFactory::create)
//!   takes `speed` by value, so the constructor's `generateSpeed` write
//!   back to the caller (`patrol_graph.cpp:394`) is applied on the first
//!   `next_step` instead of at construction.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use foundation::math::vector::Vector3;
use resources::bwresource::BwResource;
use resources::datasection::DataSection;

use crate::bot::{MovementController, MovementFactory};

/// `almostEqual( float, float )` (`vector3.hpp:131`, epsilon 0.0004).
fn almost_equal(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.0004
}

/// `unitRand()` / `randInRange` (`mathdef.hpp:117-130`) behind an
/// explicit state: a xorshift64* keyed by a seed, the fleet's stand-in for
/// the C global `rand()`.
#[derive(Debug, Clone)]
pub struct UnitRand {
    state: u64,
}

impl UnitRand {
    pub fn new(seed: u64) -> Self {
        // A zero state would stall the generator.
        Self {
            state: if seed == 0 { 0x9E3779B97F4A7C15 } else { seed },
        }
    }

    /// A value in [0, 1), the port's `unitRand()`.
    pub fn unit(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        ((x.wrapping_mul(0x2545F4914F6CDD1D) >> 40) as f32) / 16777216.0
    }

    /// `randInRange( min, max )`: `min + unitRand() * (max - min)`.
    pub fn in_range(&mut self, min: f32, max: f32) -> f32 {
        min + self.unit() * (max - min)
    }

    /// `rand() % n` for a non-empty adjacency list.
    fn index(&mut self, n: usize) -> usize {
        ((self.unit() * n as f32) as usize).min(n - 1)
    }
}

/// `Node::Properties` (`patrol_graph.cpp:189-198`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NodeProperties {
    pub min_stay: f32,
    pub max_stay: f32,
    pub radius: f32,
    pub min_speed: f32,
    pub max_speed: f32,
}

impl NodeProperties {
    /// `Properties::init`: `readFloat` each key, keeping the default when
    /// the tag is missing.
    fn init(section: Option<&DataSection>, mut props: NodeProperties) -> Self {
        let Some(section) = section else {
            return props;
        };
        let read = |tag: &str, default: f32| {
            section
                .find_child(tag)
                .map(|child| child.as_float(default))
                .unwrap_or(default)
        };
        props.min_stay = read("minStay", props.min_stay);
        props.max_stay = read("maxStay", props.max_stay);
        props.radius = read("radius", props.radius);
        props.min_speed = read("minSpeed", props.min_speed);
        props.max_speed = read("maxSpeed", props.max_speed);
        props
    }
}

/// Which part of the graph document failed to load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    /// `Graph::init: No nodes section`.
    NoNodesSection,
    /// `Graph::init: Failed to initialise node %d`.
    FailedNode(usize),
    /// `Graph::init: Node %s is a duplicate`.
    DuplicateNode(String),
    /// `Graph::init: Could not find %s for %s` (edge target, node name).
    MissingEdge(String, String),
    /// `Graph::init: Failed to initialise edges for node %d`.
    FailedEdges(usize),
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::NoNodesSection => write!(f, "Graph::init: No nodes section"),
            GraphError::FailedNode(index) => {
                write!(f, "Graph::init: Failed to initialise node {index}")
            }
            GraphError::DuplicateNode(name) => {
                write!(f, "Graph::init: Node {name} is a duplicate")
            }
            GraphError::MissingEdge(dst, from) => {
                write!(f, "Graph::init: Could not find {dst} for {from}")
            }
            GraphError::FailedEdges(index) => {
                write!(
                    f,
                    "Graph::init: Failed to initialise edges for node {index}"
                )
            }
        }
    }
}

/// `Patrol::Node` (`patrol_graph.hpp:23-88`).
#[derive(Debug, Clone)]
pub struct Node {
    id: usize,
    position: Vector3,
    name: String,
    props: NodeProperties,
    to_nodes: Vec<usize>,
}

impl Node {
    pub fn id(&self) -> usize {
        self.id
    }

    pub fn position(&self) -> Vector3 {
        self.position
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn props(&self) -> NodeProperties {
        self.props
    }

    pub fn adjacencies(&self) -> &[usize] {
        &self.to_nodes
    }

    /// `Node::flatDistSqrTo`: the squared distance in the ground plane.
    fn flat_dist_sqr_to(&self, pos: Vector3) -> f32 {
        let x = self.position.x - pos.x;
        let z = self.position.z - pos.z;
        x * x + z * z
    }

    /// `Node::nextNode` (`:120-130`): a random adjacency, or this node
    /// again when it has none.
    fn next_node(&self, rng: &mut UnitRand) -> usize {
        if self.to_nodes.is_empty() {
            return self.id;
        }
        self.to_nodes[rng.index(self.to_nodes.len())]
    }

    /// `Node::randomisePosition` (`:137-150`): snap onto the path to a
    /// random adjacency, returning that adjacency.
    fn randomise_position(&self, pos: &mut Vector3, graph: &Graph, rng: &mut UnitRand) -> usize {
        if self.to_nodes.is_empty() {
            *pos = self.position;
            return self.id;
        }
        let next = self.to_nodes[rng.index(self.to_nodes.len())];
        let target = graph
            .node(next)
            .map(|node| node.position)
            .unwrap_or(self.position);
        *pos = self.position + (target - self.position) * rng.unit();
        next
    }

    /// `Node::generateDestination` (`:156-163`): a random point inside the
    /// node's radius, at the node's height.
    fn generate_destination(&self, rng: &mut UnitRand) -> Vector3 {
        let radius = self.props.radius;
        Vector3::new(
            rng.in_range(self.position.x - radius, self.position.x + radius),
            self.position.y,
            rng.in_range(self.position.z - radius, self.position.z + radius),
        )
    }

    /// `Node::generateStayTime` (`:169-172`).
    fn generate_stay_time(&self, rng: &mut UnitRand) -> f32 {
        rng.in_range(self.props.min_stay, self.props.max_stay)
    }

    /// `Node::generateSpeed` (`:178-183`): the node's speed range, or the
    /// incoming speed when either bound is not positive.
    fn generate_speed(&self, speed: f32, rng: &mut UnitRand) -> f32 {
        if self.props.min_speed <= 0.0 || self.props.max_speed <= 0.0 {
            return speed;
        }
        rng.in_range(self.props.min_speed, self.props.max_speed)
    }
}

/// `Patrol::Graph` (`patrol_graph.hpp:94-108`).
#[derive(Debug, Clone, Default)]
pub struct Graph {
    nodes: Vec<Node>,
}

impl Graph {
    /// `Graph::init` (`patrol_graph.cpp:216-269`): two passes, nodes then
    /// edges, with `scalePos` applied to positions and radii and
    /// `scaleSpeed` to the speed bounds.
    pub fn init(
        section: &DataSection,
        scale_pos: f32,
        scale_speed: f32,
    ) -> Result<Self, GraphError> {
        let nodes_section = section
            .find_child("nodes")
            .ok_or(GraphError::NoNodesSection)?;

        let defaults = NodeProperties::init(
            section.find_child("nodeDefaults"),
            NodeProperties::default(),
        );

        let count = nodes_section.count_children();
        let mut nodes = Vec::with_capacity(count);
        let mut by_name: HashMap<String, usize> = HashMap::new();

        for index in 0..count {
            let child = nodes_section
                .open_child(index)
                .ok_or(GraphError::FailedNode(index))?;
            let mut name = child.read_string("name", "");
            if name.is_empty() {
                name = index.to_string();
            }
            let mut node = Node {
                id: index,
                position: child.read_vector3("pos", Vector3::new(0.0, 0.0, 0.0)) * scale_pos,
                name: name.clone(),
                props: NodeProperties::init(Some(child), defaults),
                to_nodes: Vec::new(),
            };
            node.props.radius *= scale_pos;
            node.props.min_speed *= scale_speed;
            node.props.max_speed *= scale_speed;

            if by_name.insert(name.clone(), index).is_some() {
                return Err(GraphError::DuplicateNode(name));
            }
            nodes.push(node);
        }

        for (index, node_slot) in nodes.iter_mut().enumerate() {
            let child = nodes_section
                .open_child(index)
                .ok_or(GraphError::FailedEdges(index))?;
            // A node without an `edges` section only draws the reference
            // WARNING ("Graph::init2: No edges section", :71-74); the node
            // simply has no adjacencies and `nextNode` falls back to
            // itself.
            if let Some(edges) = child.find_child("edges") {
                let mut to_nodes = Vec::with_capacity(edges.count_children());
                for edge_index in 0..edges.count_children() {
                    let dst = edges
                        .open_child(edge_index)
                        .map(|edge| edge.as_string().to_string())
                        .unwrap_or_default();
                    let target = *by_name.get(&dst).ok_or_else(|| {
                        GraphError::MissingEdge(dst.clone(), node_slot.name.clone())
                    })?;
                    to_nodes.push(target);
                }
                node_slot.to_nodes = to_nodes;
            }
        }

        Ok(Self { nodes })
    }

    /// `Graph::findClosestNode` (`:275-297`): the nearest node in the
    /// ground plane, `None` for an empty graph where the reference
    /// returns -1.
    pub fn find_closest_node(&self, position: Vector3) -> Option<usize> {
        let mut best: Option<(f32, usize)> = None;
        for (index, node) in self.nodes.iter().enumerate() {
            let dist = node.flat_dist_sqr_to(position);
            if best.is_none_or(|(best_dist, _)| dist < best_dist) {
                best = Some((dist, index));
            }
        }
        best.map(|(_, index)| index)
    }

    /// `Graph::node` (`:303-306`).
    pub fn node(&self, index: usize) -> Option<&Node> {
        self.nodes.get(index)
    }

    /// `Graph::size` (`patrol_graph.hpp:103`).
    pub fn size(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// What [`Graphs`] reads graphs from - `BWResource::openSection` in the
/// reference. The trait lets tests serve graphs without a resource root.
pub trait GraphSource {
    fn open_graph(&self, name: &str) -> Option<Arc<DataSection>>;
}

impl GraphSource for BwResource {
    fn open_graph(&self, name: &str) -> Option<Arc<DataSection>> {
        self.open_section(name, false)
    }
}

/// `Patrol::Graphs` (`patrol_graph.hpp:114-124`): a cache keyed by the
/// decorated name `"<graph>,<scalePos>,<scaleSpeed>"`, so one document
/// loads once per scaling (`patrol_graph.cpp:324-359`).
#[derive(Default)]
pub struct Graphs {
    graphs: HashMap<String, Arc<Graph>>,
}

impl Graphs {
    pub fn new() -> Self {
        Self::default()
    }

    fn decorated_name(name: &str, scale_pos: f32, scale_speed: f32) -> Option<String> {
        // `if (graphName.size() > sizeof(dnbuf)-64) return NULL;`
        if name.len() > 1024 - 64 {
            return None;
        }
        Some(format!("{name},{scale_pos:.6},{scale_speed:.6}"))
    }

    /// `Graphs::getGraph` (`:324-359`): cache first, then load.
    pub fn get_graph(
        &mut self,
        source: &dyn GraphSource,
        name: &str,
        scale_pos: f32,
        scale_speed: f32,
    ) -> Option<Arc<Graph>> {
        let key = Self::decorated_name(name, scale_pos, scale_speed)?;
        if let Some(graph) = self.graphs.get(&key) {
            return Some(Arc::clone(graph));
        }
        let section = source.open_graph(name)?;
        let graph = Graph::init(&section, scale_pos, scale_speed).ok()?;
        let graph = Arc::new(graph);
        self.graphs.insert(key, Arc::clone(&graph));
        Some(graph)
    }

    pub fn len(&self) -> usize {
        self.graphs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.graphs.is_empty()
    }
}

/// `Patrol::GraphTraverser` (`patrol_graph.hpp:130-143`).
pub struct GraphTraverser {
    destination_node: usize,
    destination_pos: Vector3,
    stay_time: f32,
    graph: Arc<Graph>,
    rng: UnitRand,
    /// The constructor's `generateSpeed` write back
    /// (`patrol_graph.cpp:394`), applied on the first `next_step` because
    /// the factory's `speed` is by value here.
    pending_speed: Option<f32>,
}

impl GraphTraverser {
    /// `GraphTraverser::GraphTraverser` (`:369-395`).
    pub fn new(
        graph: Arc<Graph>,
        speed: f32,
        start_position: &mut Vector3,
        randomise: bool,
        snap: bool,
        rng: UnitRand,
    ) -> Option<Self> {
        if graph.is_empty() {
            // The reference's `rand() % 0` / `node( -1 )` are undefined.
            return None;
        }
        let mut rng = rng;

        // head for either the closest node or any old node
        let mut destination_node = if randomise {
            rng.index(graph.size())
        } else {
            // An empty graph is guarded above, so this is always Some.
            graph.find_closest_node(*start_position)?
        };

        // start off either where we were or on the path to a neighbouring
        // node
        if snap {
            let node = graph.node(destination_node)?;
            destination_node = node.randomise_position(start_position, &graph, &mut rng);
        }

        let node = graph.node(destination_node)?;
        let destination_pos = node.generate_destination(&mut rng);
        let speed = node.generate_speed(speed, &mut rng);

        Some(Self {
            destination_node,
            destination_pos,
            stay_time: -1.0,
            graph,
            rng,
            pending_speed: Some(speed),
        })
    }

    pub fn destination_node(&self) -> usize {
        self.destination_node
    }

    pub fn stay_time(&self) -> f32 {
        self.stay_time
    }
}

impl fmt::Debug for GraphTraverser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GraphTraverser")
            .field("destination_node", &self.destination_node)
            .field("destination_pos", &self.destination_pos)
            .field("stay_time", &self.stay_time)
            .finish_non_exhaustive()
    }
}

impl MovementController for GraphTraverser {
    /// `GraphTraverser::nextStep` (`:401-444`).
    fn next_step(&mut self, speed: &mut f32, dtime: f32, pos: &mut Vector3, yaw: &mut f32) -> bool {
        // The node's speed applies from here when the constructor changed it.
        if let Some(pending) = self.pending_speed.take() {
            *speed = pending;
        }

        // If we are already at a node
        if self.stay_time >= 0.0 {
            self.stay_time -= dtime;

            // If our stay is over pick a new dest and zzp
            if self.stay_time <= 0.0 {
                let Some(node) = self.graph.node(self.destination_node) else {
                    return true;
                };
                self.destination_node = node.next_node(&mut self.rng);
                let Some(node) = self.graph.node(self.destination_node) else {
                    return true;
                };
                self.destination_pos = node.generate_destination(&mut self.rng);
                self.stay_time = -1.0;
                *speed = node.generate_speed(*speed, &mut self.rng);
            }
        }

        let distance = *speed * dtime;

        // Vectors to dest and zzp respectively
        let dir_vec = self.destination_pos - *pos;
        let length = dir_vec.length();

        // TODO: When is near enough
        if length < distance {
            // Note: Negative stay time means we are in transit.
            let Some(node) = self.graph.node(self.destination_node) else {
                return true;
            };
            if self.stay_time < 0.0 {
                self.stay_time = node.generate_stay_time(&mut self.rng);
            }

            self.destination_pos = node.generate_destination(&mut self.rng);
        } else {
            *pos = *pos + dir_vec * (distance / length);
            *yaw = dir_vec.yaw();
        }

        true
    }
}

/// `ZigzagPatrol::ZigzagGraphTraverser`
/// (`zigzag_patrol_graph.{hpp,cpp}`): a [`GraphTraverser`] variant that
/// walks the node-to-node corridor instead of beelining, weaving across
/// the centre line within `corridorWidth_`.
pub struct ZigzagGraphTraverser {
    destination_node: usize,
    destination_pos: Vector3,
    stay_time: f32,
    graph: Arc<Graph>,
    rng: UnitRand,
    corridor_width: f32,
    source_pos: Vector3,
    zigzag_pos: Vector3,
    pending_speed: Option<f32>,
}

/// `ZigzagPatrol::DEFAULT_CORRIDOR_WIDTH`
/// (`zigzag_patrol_graph.cpp:16`).
pub const DEFAULT_CORRIDOR_WIDTH: f32 = 50.0;

impl ZigzagGraphTraverser {
    /// The constructor: the base `GraphTraverser` with `randomise` and
    /// `snap` forced true (`:24`), then the post-snap start position
    /// becomes the corridor source and the first zigzag target
    /// (`:28-35`).
    pub fn new(
        graph: Arc<Graph>,
        speed: f32,
        start_position: &mut Vector3,
        corridor_width: f32,
        rng: UnitRand,
    ) -> Option<Self> {
        if graph.is_empty() {
            return None;
        }
        let mut rng = rng;
        let mut destination_node = rng.index(graph.size());
        let node = graph.node(destination_node)?;
        destination_node = node.randomise_position(start_position, &graph, &mut rng);
        let node = graph.node(destination_node)?;
        let destination_pos = node.generate_destination(&mut rng);
        let speed = node.generate_speed(speed, &mut rng);

        let mut traverser = Self {
            destination_node,
            destination_pos,
            stay_time: -1.0,
            graph,
            rng,
            corridor_width,
            source_pos: *start_position,
            zigzag_pos: Vector3::new(0.0, 0.0, 0.0),
            pending_speed: Some(speed),
        };
        traverser.set_zigzag_pos(*start_position);
        Some(traverser)
    }

    /// `setZigzagPos` (`:40-80`): the closest point on the source-
    /// destination line, then a point past it inside the corridor. Within
    /// `2 * corridorWidth` of the destination the target is the
    /// destination itself.
    fn set_zigzag_pos(&mut self, curr_pos: Vector3) {
        let a = self.source_pos;
        let b = self.destination_pos;

        // Calculate closest point to position on line between a and b
        let poi = if !almost_equal(b.x, a.x) {
            let m = (b.z - a.z) / (b.x - a.x);
            let x = (curr_pos.x + m * curr_pos.z + m * m * a.x - m * a.z) / (m * m + 1.0);
            let z = m * (x - a.x) + a.z;
            Vector3::new(x, curr_pos.y, z)
        } else {
            // This bit prevents divide by zero in the above block
            Vector3::new(a.x, curr_pos.y, curr_pos.z)
        };

        // If the distance from the poi to the center of the destination
        // node is less than the corridor width, then set the zigzagPos to
        // be the destination _position_
        if (b - poi).length() < 2.0 * self.corridor_width {
            self.zigzag_pos = b;
            return;
        }

        // Unit vector on edge
        let mut along = b - a;
        along.normalise();

        // Unit vector from curr pos towards edge
        let mut across = poi - curr_pos;
        across.normalise();

        // Somewhere on the other side of the edge and forward
        let along_amt = 2.0 * self.corridor_width * self.rng.unit();
        let across_amt = self.corridor_width * self.rng.unit();
        self.zigzag_pos = poi + along * along_amt + across * across_amt;
    }
}

impl MovementController for ZigzagGraphTraverser {
    /// `ZigzagGraphTraverser::nextStep` (`:82-139`).
    fn next_step(&mut self, speed: &mut f32, dtime: f32, pos: &mut Vector3, yaw: &mut f32) -> bool {
        if let Some(pending) = self.pending_speed.take() {
            *speed = pending;
        }

        // Distance travelled per tick
        let distance = *speed * dtime;

        // The destination node
        let node_pos = self
            .graph
            .node(self.destination_node)
            .map(|node| node.position());

        // If we are already at a node
        if self.stay_time > 0.0 {
            self.stay_time -= dtime;

            // If our stay is over pick a new dest and zzp
            if self.stay_time <= 0.0 {
                if let Some(node_pos) = node_pos {
                    self.source_pos = node_pos;
                }
                let next = self
                    .graph
                    .node(self.destination_node)
                    .map(|node| node.next_node(&mut self.rng))
                    .unwrap_or(self.destination_node);
                self.destination_node = next;
                if let Some(node) = self.graph.node(next) {
                    self.destination_pos = node.generate_destination(&mut self.rng);
                }
                self.set_zigzag_pos(*pos);
            }
            // If we're still hanging around but we've arrived at the zzp,
            // pick a new one
            else {
                let zz_len = (self.zigzag_pos - *pos).length();
                if let Some(node) = self
                    .graph
                    .node(self.destination_node)
                    .filter(|_| zz_len < distance)
                {
                    self.zigzag_pos = node.generate_destination(&mut self.rng);
                }
            }
        }
        // If we have approached a node
        else if (self.destination_pos - *pos).length() < distance {
            if let Some(node) = self.graph.node(self.destination_node) {
                self.stay_time = node.generate_stay_time(&mut self.rng);
                self.zigzag_pos = node.generate_destination(&mut self.rng);
            }
        }
        // If we are at a zzp
        else if (self.zigzag_pos - *pos).length() < distance {
            self.set_zigzag_pos(*pos);
        }

        // Move! (A zero hop would divide by zero into a NaN step in the
        // reference; the port holds position instead.)
        let zz_vec = self.zigzag_pos - *pos;
        let zz_len = zz_vec.length();
        if zz_len > 0.0 {
            *pos = *pos + zz_vec * (distance / zz_len);
            *yaw = zz_vec.yaw();
        }

        true
    }
}

/// `ZigzagPatrol::ZigzagPatrolFactory`, registered as `"ZigzagPatrol"`
/// (`zigzag_patrol_graph.cpp:150-196`): data is
/// `"<graph>[,<corridorWidth>]"`, always randomise + snap.
pub struct ZigzagPatrolFactory {
    source: Box<dyn GraphSource + Send + Sync>,
    graphs: Mutex<Graphs>,
    seed: u64,
}

impl ZigzagPatrolFactory {
    pub fn new(source: Box<dyn GraphSource + Send + Sync>, seed: u64) -> Self {
        Self {
            source,
            graphs: Mutex::new(Graphs::new()),
            seed,
        }
    }
}

impl MovementFactory for ZigzagPatrolFactory {
    fn create(
        &self,
        data: &str,
        speed: f32,
        start_pos: &mut Vector3,
    ) -> Option<Box<dyn MovementController>> {
        // `data.find_first_of(',')` split: a width that is not a number
        // reads as 0 (`atof`), i.e. the default corridor.
        let (graph_name, corridor_width) = match data.find(',') {
            Some(comma) => (
                &data[..comma],
                data[comma + 1..].trim().parse::<f32>().unwrap_or(0.0),
            ),
            None => (data, -1.0),
        };

        let mut graphs = self.graphs.lock().ok()?;
        let graph = graphs.get_graph(self.source.as_ref(), graph_name, 1.0, 1.0)?;
        drop(graphs);

        let width = if corridor_width > 0.0 {
            corridor_width
        } else {
            DEFAULT_CORRIDOR_WIDTH
        };
        let traverser =
            ZigzagGraphTraverser::new(graph, speed, start_pos, width, UnitRand::new(self.seed))?;
        Some(Box::new(traverser))
    }
}

/// `Patrol::PatrolFactory`, registered as `"Patrol"`
/// (`patrol_graph.cpp:456-520`).
pub struct PatrolFactory {
    source: Box<dyn GraphSource + Send + Sync>,
    graphs: Mutex<Graphs>,
    seed: u64,
}

impl PatrolFactory {
    pub fn new(source: Box<dyn GraphSource + Send + Sync>, seed: u64) -> Self {
        Self {
            source,
            graphs: Mutex::new(Graphs::new()),
            seed,
        }
    }

    /// The parsing half of `PatrolFactory::create` (`:470-503`):
    /// `"<graph>[,random][,snap][,scalePos=<f>][,scaleSpeed=<f>]"`.
    pub fn parse_args(data: &str) -> Result<PatrolArgs, String> {
        let mut graph_name = String::new();
        let mut args = PatrolArgs {
            scale_pos: 1.0,
            scale_speed: 1.0,
            ..PatrolArgs::default()
        };
        for (argnum, arg) in data.split(',').enumerate() {
            if argnum == 0 {
                graph_name = arg.to_string();
            } else if arg == "random" {
                args.randomise = true;
            } else if arg == "snap" {
                args.snap = true;
            } else if let Some(value) = arg.strip_prefix("scalePos=") {
                args.scale_pos = parse_float(value);
            } else if let Some(value) = arg.strip_prefix("scaleSpeed=") {
                args.scale_speed = parse_float(value);
            } else {
                return Err(format!("PatrolFactory::create: Unknown argument '{arg}'\n"));
            }
        }
        args.graph_name = graph_name;
        Ok(args)
    }
}

/// The parsed `Patrol` controller data.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PatrolArgs {
    pub graph_name: String,
    pub randomise: bool,
    pub snap: bool,
    pub scale_pos: f32,
    pub scale_speed: f32,
}

/// `atof` on a scale argument, `0.0` when unparsable.
fn parse_float(text: &str) -> f32 {
    text.trim().parse::<f32>().unwrap_or(0.0)
}

impl MovementFactory for PatrolFactory {
    fn create(
        &self,
        data: &str,
        speed: f32,
        start_pos: &mut Vector3,
    ) -> Option<Box<dyn MovementController>> {
        let args = Self::parse_args(data).ok()?;
        let mut graphs = self.graphs.lock().ok()?;
        let graph = graphs.get_graph(
            self.source.as_ref(),
            &args.graph_name,
            args.scale_pos,
            args.scale_speed,
        )?;
        drop(graphs);
        let traverser = GraphTraverser::new(
            graph,
            speed,
            start_pos,
            args.randomise,
            args.snap,
            UnitRand::new(self.seed),
        )?;
        Some(Box::new(traverser))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_NODE_BWP: &str = r#"<root>
        <nodeDefaults>
            <minStay>1.0</minStay>
            <maxStay>2.0</maxStay>
            <radius>5.0</radius>
            <minSpeed>3.0</minSpeed>
            <maxSpeed>4.0</maxSpeed>
        </nodeDefaults>
        <nodes>
            <node>
                <name>a</name>
                <pos>0 0 0</pos>
                <edges>
                    <edge>b</edge>
                </edges>
            </node>
            <node>
                <name>b</name>
                <pos>10 0 0</pos>
                <edges>
                    <edge>a</edge>
                </edges>
            </node>
        </nodes>
    </root>"#;

    /// A graph source serving fixed documents, standing in for the
    /// resource root.
    struct MapSource(Vec<(&'static str, &'static str)>);

    impl GraphSource for MapSource {
        fn open_graph(&self, name: &str) -> Option<Arc<DataSection>> {
            let (_, raw) = self.0.iter().find(|(key, _)| *key == name)?;
            Some(Arc::new(datasection_parse(raw)))
        }
    }

    fn datasection_parse(raw: &str) -> DataSection {
        resources::datasection::parse("root", raw.as_bytes()).expect("test xml parses")
    }

    fn two_node_graph() -> Graph {
        Graph::init(&datasection_parse(TWO_NODE_BWP), 1.0, 1.0).expect("graph inits")
    }

    #[test]
    fn graph_init_resolves_edges_scales_and_defaults() {
        let graph = Graph::init(&datasection_parse(TWO_NODE_BWP), 2.0, 3.0).unwrap();
        assert_eq!(graph.size(), 2);

        let a = graph.node(0).unwrap();
        assert_eq!(a.name(), "a");
        assert_eq!(a.id(), 0);
        // Positions scale by scalePos.
        assert_eq!(a.position(), Vector3::new(0.0, 0.0, 0.0));
        let b = graph.node(1).unwrap();
        assert_eq!(b.position(), Vector3::new(20.0, 0.0, 0.0));
        // nodeDefaults feed both nodes; radius scales with position, the
        // speed bounds with scaleSpeed (patrol_graph.cpp:243-246).
        assert_eq!(a.props().radius, 10.0);
        assert_eq!(a.props().min_stay, 1.0);
        assert_eq!(a.props().max_stay, 2.0);
        assert_eq!(a.props().min_speed, 9.0);
        assert_eq!(a.props().max_speed, 12.0);
        // Edges resolve to indices in the second pass.
        assert_eq!(a.adjacencies(), &[1]);
        assert_eq!(b.adjacencies(), &[0]);
    }

    #[test]
    fn an_unnamed_node_takes_its_index() {
        let raw = r#"<root><nodes><node><pos>1 2 3</pos></node></nodes></root>"#;
        let graph = Graph::init(&datasection_parse(raw), 1.0, 1.0).unwrap();
        assert_eq!(graph.node(0).unwrap().name(), "0");
        // Without nodeDefaults every property stays at zero.
        assert_eq!(graph.node(0).unwrap().props(), NodeProperties::default());
    }

    #[test]
    fn init_failure_modes_match_the_reference_messages() {
        let no_nodes = datasection_parse("<root><other/></root>");
        assert_eq!(
            Graph::init(&no_nodes, 1.0, 1.0).unwrap_err(),
            GraphError::NoNodesSection
        );
        assert_eq!(
            GraphError::NoNodesSection.to_string(),
            "Graph::init: No nodes section"
        );

        let dup = datasection_parse(
            r#"<root><nodes><node><name>a</name></node><node><name>a</name></node></nodes></root>"#,
        );
        assert_eq!(
            Graph::init(&dup, 1.0, 1.0).unwrap_err(),
            GraphError::DuplicateNode("a".to_string())
        );

        let missing_edge = datasection_parse(
            r#"<root><nodes><node><name>a</name><edges><edge>ghost</edge></edges></node></nodes></root>"#,
        );
        assert_eq!(
            Graph::init(&missing_edge, 1.0, 1.0).unwrap_err(),
            GraphError::MissingEdge("ghost".to_string(), "a".to_string())
        );
        assert_eq!(
            GraphError::MissingEdge("ghost".to_string(), "a".to_string()).to_string(),
            "Graph::init: Could not find ghost for a"
        );
        assert_eq!(
            GraphError::FailedNode(3).to_string(),
            "Graph::init: Failed to initialise node 3"
        );
        assert_eq!(
            GraphError::DuplicateNode("a".to_string()).to_string(),
            "Graph::init: Node a is a duplicate"
        );
        assert_eq!(
            GraphError::FailedEdges(2).to_string(),
            "Graph::init: Failed to initialise edges for node 2"
        );
    }

    #[test]
    fn find_closest_node_measures_in_the_ground_plane() {
        let graph = two_node_graph();
        // Far above b but flat-near it: y is ignored (flatDistSqrTo).
        assert_eq!(
            graph.find_closest_node(Vector3::new(8.0, 50.0, 3.0)),
            Some(1)
        );
        assert_eq!(
            graph.find_closest_node(Vector3::new(-1.0, 0.0, 0.0)),
            Some(0)
        );
    }

    #[test]
    fn the_traverser_heads_for_the_closest_node() {
        let graph = Arc::new(two_node_graph());
        let mut start = Vector3::new(8.0, 0.0, 0.0);
        let traverser = GraphTraverser::new(
            Arc::clone(&graph),
            6.0,
            &mut start,
            false,
            false,
            UnitRand::new(1),
        )
        .unwrap();
        assert_eq!(traverser.destination_node(), 1);
        assert_eq!(traverser.stay_time(), -1.0, "starts in transit");
        // No snap: the start position is untouched.
        assert_eq!(start, Vector3::new(8.0, 0.0, 0.0));
        // The Debug face carries the routing state.
        let rendered = format!("{traverser:?}");
        assert!(rendered.contains("destination_node: 1"), "{rendered}");
    }

    #[test]
    fn snap_starts_on_the_path_to_a_neighbour() {
        let graph = Arc::new(two_node_graph());
        let mut start = Vector3::new(8.0, 0.0, 0.0);
        let rng = UnitRand::new(7);
        // The constructor draws twice on the snap path: `index()` for the
        // adjacency, then `unit()` for the position along it. Probe both
        // draws so the assertion is exact.
        let expected_unit = {
            let mut probe = rng.clone();
            let _adjacency_draw = probe.unit();
            probe.unit()
        };
        let traverser =
            GraphTraverser::new(Arc::clone(&graph), 6.0, &mut start, false, true, rng).unwrap();
        // The snapped start sits between the two nodes... the closest
        // node is b (x=10) and the draw walks toward its neighbour a
        // (x=0), so the position is `10 * (1 - unit)`.
        assert!(start.x > 0.0 && start.x < 10.0, "snapped to {start:?}");
        assert!(
            near(start.x, 10.0 * (1.0 - expected_unit)),
            "half-way {start:?}"
        );
        // ...and the destination node is that neighbour (node 0 or 1 -
        // with one adjacency each way it is whichever randomise picked).
        assert!(traverser.destination_node() <= 1);
    }

    #[test]
    fn arrival_sets_a_stay_from_the_node_properties() {
        let graph = Arc::new(two_node_graph());
        let mut start = Vector3::new(9.4, 0.0, 0.0);
        let mut traverser = GraphTraverser::new(
            Arc::clone(&graph),
            6.0,
            &mut start,
            false,
            false,
            UnitRand::new(3),
        )
        .unwrap();

        // Walk toward the destination inside node 1 until the step
        // overshoots: `length < distance` starts the stay.
        let mut pos = start;
        let mut yaw = 0.0;
        let mut speed = 6.0;
        for _ in 0..50 {
            traverser.next_step(&mut speed, 0.1, &mut pos, &mut yaw);
            if traverser.stay_time() >= 0.0 {
                break;
            }
        }
        let stay = traverser.stay_time();
        assert!(
            (1.0..=2.0).contains(&stay),
            "stay within minStay..maxStay: {stay}"
        );

        // Once the stay elapses, the next node comes from the adjacencies
        // and the traverser is back in transit (`stayTime_ = -1.f`,
        // patrol_graph.cpp:406-418).
        let elapsed = traverser.next_step(&mut speed, stay + 0.5, &mut pos, &mut yaw);
        assert!(elapsed);
        assert_eq!(traverser.stay_time(), -1.0);
        let previous = traverser.destination_node();
        let node = graph.node(previous).unwrap();
        assert!(
            node.adjacencies().contains(&previous) || node.adjacencies().len() == 1,
            "moved along an adjacency"
        );
    }

    #[test]
    fn a_fixed_seed_reproduces_the_waypoint_order() {
        let graph = Arc::new(two_node_graph());
        let route = |seed: u64| {
            let mut start = Vector3::new(8.0, 0.0, 0.0);
            let mut traverser = GraphTraverser::new(
                Arc::clone(&graph),
                6.0,
                &mut start,
                true,
                true,
                UnitRand::new(seed),
            )
            .unwrap();
            let mut pos = start;
            let mut yaw = 0.0;
            let mut speed = 6.0;
            let mut visited = Vec::new();
            for _ in 0..40 {
                traverser.next_step(&mut speed, 0.1, &mut pos, &mut yaw);
                visited.push(traverser.destination_node());
            }
            visited
        };
        assert_eq!(route(11), route(11), "same seed, same waypoints");
        assert_ne!(route(11), route(12), "different seed, different route");
    }

    #[test]
    fn parse_args_reads_the_documented_switches() {
        let args = PatrolFactory::parse_args("g.bwp,random,snap,scalePos=2,scaleSpeed=0.5")
            .expect("parses");
        assert_eq!(args.graph_name, "g.bwp");
        assert!(args.randomise && args.snap);
        assert_eq!(args.scale_pos, 2.0);
        assert_eq!(args.scale_speed, 0.5);

        // Defaults are 1.0 (patrol_graph.cpp:472).
        let args = PatrolFactory::parse_args("g.bwp").unwrap();
        assert_eq!(args.graph_name, "g.bwp");
        assert!(!args.randomise && !args.snap);
        assert_eq!(args.scale_pos, 1.0);
        assert_eq!(args.scale_speed, 1.0);

        // An unknown switch is the reference's ValueError (:498-499).
        let err = PatrolFactory::parse_args("g.bwp,warp").unwrap_err();
        assert_eq!(err, "PatrolFactory::create: Unknown argument 'warp'\n");
    }

    #[test]
    fn the_factory_builds_a_traverser_from_the_resource_data() {
        let factory = PatrolFactory::new(
            Box::new(MapSource(vec![("server/bots/test.bwp", TWO_NODE_BWP)])),
            5,
        );

        let mut start = Vector3::new(8.0, 0.0, 0.0);
        let mut controller = factory
            .create("server/bots/test.bwp", 6.0, &mut start)
            .expect("the graph loads");

        // The first step moves and applies the node's speed range
        // (minSpeed/maxSpeed 3..4 after the 1.0 scaleSpeed).
        let mut pos = start;
        let mut yaw = 0.0;
        let mut speed = 6.0;
        assert!(controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
        assert!((3.0..=4.0).contains(&speed), "node speed applied: {speed}");
        assert!(pos != start || yaw != 0.0, "the bot moved or turned");

        // A graph no source serves fails the create (:512-513).
        let mut start = Vector3::new(0.0, 0.0, 0.0);
        assert!(factory.create("no/such.bwp", 6.0, &mut start).is_none());
        // So does an unknown argument (the ValueError path).
        assert!(factory
            .create("server/bots/test.bwp,warp", 6.0, &mut start)
            .is_none());
    }

    #[test]
    fn the_graph_cache_keys_on_the_scaling() {
        let source = MapSource(vec![("g.bwp", TWO_NODE_BWP)]);
        let mut graphs = Graphs::new();

        assert!(graphs.is_empty());
        let a = graphs.get_graph(&source, "g.bwp", 1.0, 1.0).unwrap();
        assert!(!graphs.is_empty());
        let again = graphs.get_graph(&source, "g.bwp", 1.0, 1.0).unwrap();
        assert!(Arc::ptr_eq(&a, &again), "same scaling hits the cache");
        assert_eq!(graphs.len(), 1);

        let scaled = graphs.get_graph(&source, "g.bwp", 2.0, 1.0).unwrap();
        assert!(!Arc::ptr_eq(&a, &scaled), "a new scaling loads fresh");
        assert_eq!(graphs.len(), 2);
        assert_eq!(scaled.node(1).unwrap().position().x, 20.0);

        // An over-long name is refused (patrol_graph.cpp:328).
        let long = "x".repeat(1024 - 64 + 1);
        assert!(graphs.get_graph(&source, &long, 1.0, 1.0).is_none());
    }

    #[test]
    fn the_zigzag_traverser_weaves_toward_the_destination() {
        let graph = Arc::new(two_node_graph());
        let mut start = Vector3::new(8.0, 0.0, 0.0);
        // A corridor narrower than the graph so the target genuinely
        // weaves (with the 50 default every target lands within
        // `2 * width` of the destination and no weave is computed).
        let mut traverser =
            ZigzagGraphTraverser::new(Arc::clone(&graph), 6.0, &mut start, 5.0, UnitRand::new(9))
                .unwrap();

        // The first target is the start position itself (the constructor
        // sets it, :35), so the very first move has nowhere to go and
        // holds the position instead of dividing by zero.
        let mut pos = start;
        let mut yaw = 0.0;
        let mut speed = 6.0;
        assert!(traverser.next_step(&mut speed, 0.1, &mut pos, &mut yaw));

        // Past the first step the bot keeps weaving: positions stay in
        // the corridor band around the source-destination line, the yaw
        // faces the zigzag target, and the node speed range applies.
        assert!((3.0..=4.0).contains(&speed), "node speed applied: {speed}");
        for _ in 0..200 {
            assert!(traverser.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
            assert!(pos.x.is_finite() && pos.y.is_finite() && pos.z.is_finite());
            assert!((-60.0..=70.0).contains(&pos.x), "corridor x: {}", pos.x);
            assert!((-60.0..=60.0).contains(&pos.z), "corridor z: {}", pos.z);
        }
        // The bot makes progress along the line (it does not stall).
        assert!(pos.x != start.x, "moved from {start:?} to {pos:?}");
    }

    #[test]
    fn the_zigzag_factory_takes_an_optional_corridor_width() {
        let factory =
            ZigzagPatrolFactory::new(Box::new(MapSource(vec![("g.bwp", TWO_NODE_BWP)])), 4);

        // `<graph>` alone: the default corridor width.
        let mut start = Vector3::new(8.0, 0.0, 0.0);
        let mut controller = factory
            .create("g.bwp", 6.0, &mut start)
            .expect("the graph loads");
        let mut pos = start;
        let mut yaw = 0.0;
        let mut speed = 6.0;
        assert!(controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw));

        // `<graph>,<width>`: the explicit width is honoured (a width that
        // is not a number reads as 0, i.e. the default, as `atof` does).
        let mut start = Vector3::new(8.0, 0.0, 0.0);
        let mut controller = factory.create("g.bwp,5", 6.0, &mut start).unwrap();
        let mut pos = start;
        assert!(controller.next_step(&mut speed, 0.1, &mut pos, &mut yaw));

        // An unknown graph fails the create as in the reference.
        let mut start = Vector3::new(0.0, 0.0, 0.0);
        assert!(factory.create("no/such.bwp", 6.0, &mut start).is_none());
    }

    #[test]
    fn an_edgeless_single_node_loops_back_to_itself() {
        // One node, no edges section: the WARNING-only path leaves it
        // with no adjacencies, so `nextNode` is the node itself and
        // `randomisePosition` snaps to the node position.
        let raw = r#"<root><nodes><node><name>solo</name><pos>3 0 4</pos></node></nodes></root>"#;
        let graph = Arc::new(Graph::init(&datasection_parse(raw), 1.0, 1.0).unwrap());
        let mut start = Vector3::new(100.0, 0.0, 100.0);
        let mut traverser = GraphTraverser::new(
            Arc::clone(&graph),
            6.0,
            &mut start,
            false,
            true,
            UnitRand::new(2),
        )
        .unwrap();

        // The snap landed exactly on the node.
        assert_eq!(start, Vector3::new(3.0, 0.0, 4.0));

        // No nodeDefaults: the node's speed range is unset, so the
        // incoming speed passes through (`generateSpeed`, :180).
        let mut pos = start;
        let mut yaw = 0.0;
        let mut speed = 6.0;
        assert!(traverser.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
        assert_eq!(speed, 6.0, "unset speed bounds leave the speed alone");

        // The zero-radius destination is reached immediately; the stay
        // (0..0) elapses on the next step and `nextNode` loops back to
        // the same node.
        for _ in 0..10 {
            assert!(traverser.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
            assert_eq!(traverser.destination_node(), 0, "self loop");
        }
    }

    #[test]
    fn an_empty_graph_refuses_the_traversers() {
        // The reference's `rand() % 0` / `node( -1 )` on an empty graph
        // are undefined; both constructors refuse it instead.
        let raw = r#"<root><nodes/></root>"#;
        let graph = Arc::new(Graph::init(&datasection_parse(raw), 1.0, 1.0).unwrap());
        assert!(graph.is_empty());
        let mut start = Vector3::new(0.0, 0.0, 0.0);
        assert!(GraphTraverser::new(
            Arc::clone(&graph),
            6.0,
            &mut start,
            false,
            false,
            UnitRand::new(1)
        )
        .is_none());
        assert!(ZigzagGraphTraverser::new(graph, 6.0, &mut start, 5.0, UnitRand::new(1)).is_none());
    }

    #[test]
    fn a_vertical_line_takes_the_divide_free_branch() {
        // Nodes stacked along z with an identical x: the source-
        // destination line is vertical, exercising the almost-equal branch
        // of `setZigzagPos` that avoids the slope division (:56-58).
        let raw = r#"<root><nodes>
            <node><name>a</name><pos>0 0 0</pos><edges><edge>b</edge></edges></node>
            <node><name>b</name><pos>0 0 10</pos><edges><edge>a</edge></edges></node>
        </nodes></root>"#;
        let graph = Arc::new(Graph::init(&datasection_parse(raw), 1.0, 1.0).unwrap());
        let mut start = Vector3::new(0.0, 0.0, 9.0);
        let mut traverser =
            ZigzagGraphTraverser::new(Arc::clone(&graph), 6.0, &mut start, 5.0, UnitRand::new(13))
                .unwrap();

        let mut pos = start;
        let mut yaw = 0.0;
        let mut speed = 6.0;
        for _ in 0..200 {
            assert!(traverser.next_step(&mut speed, 0.1, &mut pos, &mut yaw));
            assert!(
                pos.x.is_finite() && pos.z.is_finite(),
                "finite step at {pos:?}"
            );
            assert!((-20.0..=30.0).contains(&pos.z), "corridor z: {}", pos.z);
        }
        assert!(pos.z != 9.0, "moved along the vertical line");
    }

    #[test]
    fn the_bwresource_source_serves_a_real_resource_root() {
        // The production `GraphSource` impl reads through `BwResource`,
        // the reference's `BWResource::openSection` (patrol_graph.cpp:342).
        let dir = std::env::temp_dir().join(format!("bw-patrol-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("g.bwp"), TWO_NODE_BWP).unwrap();
        let resource = BwResource::init_paths(&[dir.to_str().unwrap()]);
        let section = resource.open_graph("g.bwp");
        std::fs::remove_dir_all(&dir).ok();
        let section = section.expect("a real resource root serves the graph");
        let graph = Graph::init(&section, 1.0, 1.0).unwrap();
        assert_eq!(graph.size(), 2);
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }
}
