//! The `Patrol` movement controller (`server/tools/bots/patrol_graph.{hpp,cpp}`).
//!
//! Bots on patrol follow a waypoint graph loaded from a `.bwp` XML
//! resource: nodes carry a position, optional stay/speed/radius
//! properties (with `<nodeDefaults>` at the root), and edges naming
//! their successors. The [`GraphTraverser`] walks the graph — it heads
//! for a destination picked around the current node, waits out the
//! node's stay time, then draws the next node from its adjacencies and
//! repeats, wrapping around whatever loops the graph wires
//! (patrol_graph.cpp:401-444).
//!
//! The reference draws randomness from two distinct globals: the C
//! `rand()` (seeded from `timestamp()`, main_app.cpp:141) picks
//! adjacencies and the snap fraction, while the Mersenne `bw_random()`
//! singleton serves `randInRange` for destinations, stay times and
//! speeds. The port runs one seeded xorshift64* stream per factory for
//! the seed draws and one per traverser for the walk — the same draws in
//! the same order, split into per-bot streams so a fixed fleet seed
//! reproduces every bot's trajectory (the BW-17-02-04 face).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use foundation::math::vector::Vector3;
use resources::bwresource::BwResource;
use resources::datasection::DataSection;

use crate::bot::{MovementController, MovementFactory};

/// The RNG behind the patrol draws — a stand-in for the reference's C
/// `rand()` and Mersenne `bw_random()` globals, split into per-factory
/// (seed) and per-traverser (walk) streams.
pub struct PatrolRandom {
    state: u64,
}

impl PatrolRandom {
    /// A zero state would stall the generator.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// The `bw_random()` stand-in: a full `u32`.
    fn bw_rand(&mut self) -> u32 {
        self.next_u64() as u32
    }

    /// The `rand()` stand-in: the top 31 bits, so the value stays within
    /// `[0, RAND_MAX]` the way the C runtime's `rand()` does.
    fn c_rand(&mut self) -> u32 {
        (self.next_u64() >> 33) as u32
    }

    /// `RAND_MAX` on this platform: `2^31 - 1`.
    const RAND_MAX: u32 = u32::MAX >> 1;

    /// `unitRand` (mathdef.hpp:118-120): `bw_random() / (uint32)-1`.
    fn unit(&mut self) -> f32 {
        self.bw_rand() as f32 / u32::MAX as f32
    }

    /// `randInRange` (mathdef.hpp:126-129): `min + unitRand() * (max-min)`.
    fn rand_in_range(&mut self, min: f32, max: f32) -> f32 {
        min + self.unit() * (max - min)
    }

    /// `rand() % n` — the reference's adjacency pick (`n >= 1`).
    fn pick(&mut self, n: usize) -> usize {
        (self.c_rand() % n as u32) as usize
    }

    /// Draw a seed for a fresh per-bot stream.
    fn draw_seed(&mut self) -> u64 {
        self.next_u64()
    }
}

/// `Node::Properties` (patrol_graph.hpp:26-53): the stay/speed/radius
/// knobs a node carries, pre-filled from the graph's `<nodeDefaults>`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct NodeProperties {
    min_stay: f32,
    max_stay: f32,
    radius: f32,
    min_speed: f32,
    max_speed: f32,
}

impl NodeProperties {
    pub const fn min_stay(&self) -> f32 {
        self.min_stay
    }
    pub const fn max_stay(&self) -> f32 {
        self.max_stay
    }
    pub const fn radius(&self) -> f32 {
        self.radius
    }
    pub const fn min_speed(&self) -> f32 {
        self.min_speed
    }
    pub const fn max_speed(&self) -> f32 {
        self.max_speed
    }

    /// `Node::Properties::init` (patrol_graph.cpp:189-198): each field
    /// reads its tag, keeping the current value (the graph defaults)
    /// when the tag is absent.
    fn init_from(&mut self, section: &DataSection) {
        self.min_stay = read_float(section, "minStay", self.min_stay);
        self.max_stay = read_float(section, "maxStay", self.max_stay);
        self.radius = read_float(section, "radius", self.radius);
        self.min_speed = read_float(section, "minSpeed", self.min_speed);
        self.max_speed = read_float(section, "maxSpeed", self.max_speed);
    }
}

/// `DataSection::readFloat`: find the child, convert or keep the default.
fn read_float(section: &DataSection, tag: &str, default: f32) -> f32 {
    section
        .find_child(tag)
        .map_or(default, |child| child.as_float(default))
}

/// `Patrol::Node` (patrol_graph.hpp:23-88): a waypoint with its edges.
pub struct Node {
    /// `toNodes_` — the indices of the successors.
    to_nodes: Vec<usize>,
    position: Vector3,
    id: usize,
    name: String,
    props: NodeProperties,
}

impl Node {
    /// `Node::init1` (patrol_graph.cpp:34-60): read the position, the
    /// name (an unnamed node takes its index as its name) and the local
    /// property overrides.
    fn init1(&mut self, index: usize, section: &DataSection, default_props: &NodeProperties) {
        self.id = index;
        self.position = section.read_vector3("pos", Vector3::ZERO);
        self.name = section.read_string("name", "");
        self.props = *default_props;
        self.props.init_from(section);
        if self.name.is_empty() {
            self.name = index.to_string();
        }
    }

    /// `Node::init2` (patrol_graph.cpp:62-94): resolve the edge
    /// children's values (destination names) against the node map. A
    /// missing `<edges>` section only warns in the reference — the node
    /// keeps no adjacencies and a walk parks on it.
    fn init2(&mut self, map: &HashMap<String, usize>, section: &DataSection) -> bool {
        let num_edges = section
            .open_section("edges")
            .map_or(0, |edges| edges.count_children());
        self.to_nodes = Vec::with_capacity(num_edges);
        if let Some(edges) = section.open_section("edges") {
            for i in 0..num_edges {
                let dst_name = edges
                    .open_child(i)
                    .expect("child index counted")
                    .as_string();
                match map.get(dst_name) {
                    Some(&index) => self.to_nodes.push(index),
                    // Could not find the destination for this node.
                    None => return false,
                }
            }
        }
        true
    }

    /// `Node::flatDistSqrTo` (patrol_graph.cpp:100-105): the squared
    /// x/z distance — the graph is navigated flat.
    fn flat_dist_sqr_to(&self, pos: &Vector3) -> f32 {
        let x = self.position.x - pos.x;
        let z = self.position.z - pos.z;
        x * x + z * z
    }

    /// `Node::nextNode` (patrol_graph.cpp:120-130): a uniform pick over
    /// the adjacencies; a node without adjacencies keeps to itself.
    fn next_node(&self, rng: &mut PatrolRandom) -> usize {
        if self.to_nodes.is_empty() {
            self.id
        } else {
            self.to_nodes[rng.pick(self.to_nodes.len())]
        }
    }

    /// `Node::randomisePosition` (patrol_graph.cpp:137-150): land the
    /// position somewhere on the path to one of the adjacencies and
    /// return the adjacency chosen.
    fn randomise_position(
        &self,
        pos: &mut Vector3,
        graph: &Graph,
        rng: &mut PatrolRandom,
    ) -> usize {
        if self.to_nodes.is_empty() {
            *pos = self.position;
            return self.id;
        }
        let next = self.to_nodes[rng.pick(self.to_nodes.len())];
        let frac = rng.c_rand() as f32 / PatrolRandom::RAND_MAX as f32;
        *pos = self.position + (graph.node(next).position - self.position) * frac;
        next
    }

    /// `Node::generateDestination` (patrol_graph.cpp:156-163): a point
    /// uniformly within the node's radius, on its own y.
    fn generate_destination(&self, rng: &mut PatrolRandom) -> Vector3 {
        let radius = self.props.radius;
        Vector3::new(
            rng.rand_in_range(self.position.x - radius, self.position.x + radius),
            self.position.y,
            rng.rand_in_range(self.position.z - radius, self.position.z + radius),
        )
    }

    /// `Node::generateStayTime` (patrol_graph.cpp:169-172).
    fn generate_stay_time(&self, rng: &mut PatrolRandom) -> f32 {
        rng.rand_in_range(self.props.min_stay, self.props.max_stay)
    }

    /// `Node::generateSpeed` (patrol_graph.cpp:178-183): a node without
    /// a speed range passes the incoming speed through.
    fn generate_speed(&self, rng: &mut PatrolRandom, ospeed: f32) -> f32 {
        if self.props.min_speed <= 0.0 || self.props.max_speed <= 0.0 {
            ospeed
        } else {
            rng.rand_in_range(self.props.min_speed, self.props.max_speed)
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn position(&self) -> Vector3 {
        self.position
    }

    pub const fn props(&self) -> &NodeProperties {
        &self.props
    }
}

/// `Patrol::Graph` (patrol_graph.hpp:94-108): the loaded waypoint graph.
#[derive(Default)]
pub struct Graph {
    nodes: Vec<Node>,
}

impl Graph {
    /// `Graph::init` (patrol_graph.cpp:216-269): read `<nodes>` (each
    /// child one node), scale the positions and the speed ranges, refuse
    /// duplicate names, then resolve every node's edges. Returns false
    /// for a missing `<nodes>` section, a duplicate name or an
    /// unresolvable edge.
    pub fn init(&mut self, section: &DataSection, scale_pos: f32, scale_speed: f32) -> bool {
        let Some(nodes) = section.find_child("nodes") else {
            // No nodes section.
            return false;
        };

        let mut defaults = NodeProperties::default();
        if let Some(node_defaults) = section.find_child("nodeDefaults") {
            defaults.init_from(node_defaults);
        }

        let num_nodes = nodes.count_children();
        self.nodes = Vec::with_capacity(num_nodes);
        let mut map: HashMap<String, usize> = HashMap::with_capacity(num_nodes);

        for i in 0..num_nodes {
            // The index comes from count_children, so it always opens.
            let child = nodes.open_child(i).expect("child index counted");
            let mut node = Node {
                to_nodes: Vec::new(),
                position: Vector3::ZERO,
                id: i,
                name: String::new(),
                props: NodeProperties::default(),
            };
            node.init1(i, child, &defaults);
            node.position = node.position * scale_pos;
            node.props.radius *= scale_pos;
            node.props.min_speed *= scale_speed;
            node.props.max_speed *= scale_speed;

            if map.insert(node.name.clone(), i).is_some() {
                // The node is a duplicate.
                return false;
            }
            self.nodes.push(node);
        }

        for (i, node) in self.nodes.iter_mut().enumerate() {
            let child = nodes.open_child(i).expect("index counted above");
            if !node.init2(&map, child) {
                // Failed to initialise the edges.
                return false;
            }
        }

        true
    }

    /// `Graph::findClosestNode` (patrol_graph.cpp:275-297): the flat
    /// nearest node, or -1 for an empty graph.
    pub fn find_closest_node(&self, position: &Vector3) -> i32 {
        let Some(first) = self.nodes.first() else {
            return -1;
        };
        let mut best_dist = first.flat_dist_sqr_to(position);
        let mut best_index = 0;
        for (i, node) in self.nodes.iter().enumerate().skip(1) {
            let dist = node.flat_dist_sqr_to(position);
            if dist < best_dist {
                best_dist = dist;
                best_index = i;
            }
        }
        best_index as i32
    }

    /// `Graph::node`: the node with the index.
    pub fn node(&self, index: usize) -> &Node {
        &self.nodes[index]
    }

    /// `Graph::size`.
    pub fn size(&self) -> usize {
        self.nodes.len()
    }
}

/// `Patrol::Graphs` (patrol_graph.hpp:114-124): the per-factory graph
/// cache, keyed by the `%f`-decorated "name,scalePos,scaleSpeed".
#[derive(Default)]
pub struct Graphs {
    graphs: HashMap<String, Arc<Graph>>,
}

impl Graphs {
    pub fn new() -> Self {
        Self::default()
    }

    /// `Graphs::getGraph` (patrol_graph.cpp:324-359): serve the cached
    /// graph, else purge and reload the resource, init and keep it. A
    /// graph that fails to init is dropped and never cached.
    pub fn get_graph(
        &mut self,
        graph_name: &str,
        scale_pos: f32,
        scale_speed: f32,
        resource: &BwResource,
    ) -> Option<Arc<Graph>> {
        // The reference formats into a 1 KiB buffer and refuses names
        // that could overflow it (64 bytes of headroom).
        if graph_name.len() > 1024 - 64 {
            return None;
        }

        let decorated = format!("{graph_name},{scale_pos:.6},{scale_speed:.6}");
        if let Some(graph) = self.graphs.get(&decorated) {
            return Some(Arc::clone(graph));
        }

        resource.purge(graph_name, false);
        let section = resource.open_section(graph_name, false)?;

        let mut graph = Graph::default();
        if !graph.init(&section, scale_pos, scale_speed) {
            return None;
        }
        let graph = Arc::new(graph);
        self.graphs.insert(decorated, Arc::clone(&graph));
        Some(graph)
    }
}

/// `Patrol::GraphTraverser` (patrol_graph.hpp:130-143): the walk itself.
pub struct GraphTraverser {
    destination_node: usize,
    destination_pos: Vector3,
    stay_time: f32,
    graph: Arc<Graph>,
    rng: PatrolRandom,
}

impl GraphTraverser {
    /// `GraphTraverser::GraphTraverser` (patrol_graph.cpp:369-395): head
    /// for the closest node (or a uniformly random one with
    /// `randomise`), optionally snapping the start position onto the
    /// path to a neighbouring node, then draw the first destination and
    /// speed around the chosen node.
    pub fn new(
        graph: Arc<Graph>,
        speed: &mut f32,
        start_position: &mut Vector3,
        randomise: bool,
        snap: bool,
        mut rng: PatrolRandom,
    ) -> Self {
        // An empty graph leaves the reference dividing by zero and
        // dereferencing node( -1 ); the factory refuses to build a
        // traverser for one before getting here.
        let mut destination_node = if randomise {
            rng.pick(graph.size())
        } else {
            graph.find_closest_node(start_position) as usize
        };

        let node = graph.node(destination_node);
        if snap {
            destination_node = node.randomise_position(start_position, &graph, &mut rng);
        }
        let node = graph.node(destination_node);

        let destination_pos = node.generate_destination(&mut rng);
        *speed = node.generate_speed(&mut rng, *speed);

        Self {
            destination_node,
            destination_pos,
            stay_time: -1.0,
            graph,
            rng,
        }
    }

    /// The node the traverser currently heads for.
    pub fn destination_node(&self) -> usize {
        self.destination_node
    }
}

impl MovementController for GraphTraverser {
    /// `GraphTraverser::nextStep` (patrol_graph.cpp:401-444): count down
    /// the stay time and, the moment it runs out, retarget to a
    /// successor and redraw the speed — the very same step then moves
    /// toward the new destination. Arrival (`length < distance`) parks
    /// the bot: the first arrival starts the stay, and every arrival
    /// redraws the destination (so a positive radius wanders while
    /// waiting).
    fn next_step(&mut self, speed: &mut f32, dtime: f32, pos: &mut Vector3, yaw: &mut f32) -> bool {
        let mut node = self.graph.node(self.destination_node);

        if self.stay_time >= 0.0 {
            self.stay_time -= dtime;

            if self.stay_time <= 0.0 {
                self.destination_node = node.next_node(&mut self.rng);
                node = self.graph.node(self.destination_node);
                self.destination_pos = node.generate_destination(&mut self.rng);
                self.stay_time = -1.0;
                *speed = node.generate_speed(&mut self.rng, *speed);
            }
        }

        let distance = *speed * dtime;

        let dir_vec = self.destination_pos - *pos;
        let length = dir_vec.length();

        if length < distance {
            // A negative stay time means we are in transit.
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

/// `PatrolFactory::create`'s argument parse (patrol_graph.cpp:468-503):
/// the first comma-separated field is the graph name, the rest are
/// flags — `random`, `snap`, `scalePos=` and `scaleSpeed=`.
///
/// The reference's do-while walks `argbeg = argend + 1` past each
/// comma, so a trailing comma adds no final empty field. Unknown fields
/// leave a pending Python error that nothing on the bots path prints
/// while the parse continues; the port swallows them the same way.
fn parse_factory_args(data: &str) -> (String, bool, bool, f32, f32) {
    let mut graph_name = String::new();
    let mut randomise = false;
    let mut snap = false;
    let mut scale_pos = 1.0;
    let mut scale_speed = 1.0;

    let mut argbeg = 0usize;
    let mut argnum = 0usize;
    loop {
        let argend = match data[argbeg..].find(',') {
            Some(rel) if argbeg + rel < data.len() => argbeg + rel,
            _ => data.len(),
        };
        let argstr = &data[argbeg..argend];

        if argnum == 0 {
            graph_name = argstr.to_string();
        } else if argstr == "random" {
            randomise = true;
        } else if argstr == "snap" {
            snap = true;
        } else if let Some(value) = argstr.strip_prefix("scalePos=") {
            scale_pos = atof(value);
        } else if let Some(value) = argstr.strip_prefix("scaleSpeed=") {
            scale_speed = atof(value);
        }

        argbeg = argend + 1;
        argnum += 1;
        if argbeg >= data.len() {
            break;
        }
    }

    (graph_name, randomise, snap, scale_pos, scale_speed)
}

/// `atof` for a flag value: a number, or 0.0 when it does not parse.
fn atof(text: &str) -> f32 {
    text.trim().parse().unwrap_or(0.0)
}

struct PatrolFactoryInner {
    graphs: Graphs,
    resource: BwResource,
    /// The `srand( timestamp() )` analogue: the shared stream the
    /// per-bot seeds are drawn from.
    seed_rng: PatrolRandom,
}

/// `PatrolFactory` (patrol_graph.cpp:456-520): the `"Patrol"` entry for
/// `moveType`. The move data is the graph resource path plus flags.
pub struct PatrolFactory {
    inner: Mutex<PatrolFactoryInner>,
}

impl PatrolFactory {
    pub fn new(resource: BwResource) -> Self {
        // The reference seeds from the wall clock (main_app.cpp:141).
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        Self::with_seed(resource, seed)
    }

    /// A fixed seed draws a fixed sequence of per-bot trajectories.
    pub fn with_seed(resource: BwResource, seed: u64) -> Self {
        Self {
            inner: Mutex::new(PatrolFactoryInner {
                graphs: Graphs::new(),
                resource,
                seed_rng: PatrolRandom::with_seed(seed),
            }),
        }
    }
}

impl MovementFactory for PatrolFactory {
    /// `PatrolFactory::create` (patrol_graph.cpp:467-514): parse the
    /// flags, load (or serve) the graph, build the traverser. A graph
    /// that cannot be loaded leaves a pending `IOError` in the
    /// reference and yields no controller; an empty one dereferences
    /// `node( -1 )` — the port refuses both the same way.
    fn create(
        &self,
        data: &str,
        speed: &mut f32,
        start_pos: &mut Vector3,
    ) -> Option<Box<dyn MovementController>> {
        let (graph_name, randomise, snap, scale_pos, scale_speed) = parse_factory_args(data);

        let mut inner = self.inner.lock().expect("patrol factory mutex poisoned");
        let PatrolFactoryInner {
            graphs,
            resource,
            seed_rng,
        } = &mut *inner;

        let graph = graphs.get_graph(&graph_name, scale_pos, scale_speed, resource)?;
        if graph.size() == 0 {
            return None;
        }

        let rng = PatrolRandom::with_seed(seed_rng.draw_seed());
        Some(Box::new(GraphTraverser::new(
            graph, speed, start_pos, randomise, snap, rng,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::ControllerRegistry;
    use resources::datasection::parse;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_root(tag: &str) -> String {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "boundless_patrol_{}_{}_{}",
            tag,
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.to_string_lossy().into_owned()
    }

    fn write(root: &str, rel: &str, data: &str) {
        let full = format!("{}{}", root.trim_end_matches('/'), rel);
        let parent = std::path::Path::new(&full).parent().expect("rooted path");
        std::fs::create_dir_all(parent).unwrap();
        std::fs::write(full, data).unwrap();
    }

    /// A three-node loop A -> B -> C -> A with zero stay, zero radius
    /// and no speed ranges: every stop is exact and the walk is fully
    /// deterministic.
    const CYCLE_XML: &str = r#"<patrol>
  <nodeDefaults>
    <minStay> 0 </minStay>
    <maxStay> 0 </maxStay>
    <radius> 0 </radius>
  </nodeDefaults>
  <nodes>
    <node> <name> A </name> <pos> 0 0 0 </pos>
      <edges> <edge> B </edge> </edges> </node>
    <node> <name> B </name> <pos> 10 0 0 </pos>
      <edges> <edge> C </edge> </edges> </node>
    <node> <name> C </name> <pos> 20 0 0 </pos>
      <edges> <edge> A </edge> </edges> </node>
  </nodes>
</patrol>"#;

    fn cycle_graph() -> Arc<Graph> {
        let section = parse("patrol", CYCLE_XML.as_bytes()).unwrap();
        let mut graph = Graph::default();
        assert!(graph.init(&section, 1.0, 1.0));
        Arc::new(graph)
    }

    /// The reference fixed walk (patrol_graph.cpp:401-444): from the
    /// origin at speed 2, dTime 1, the traverser stops a zero-stay tick
    /// on A, walks the 10-unit legs in five strides each, and the C->A
    /// edge wraps the loop — after A the walk sets out for B again.
    #[test]
    fn waypoints_are_visited_in_order_and_the_loop_wraps() {
        let mut speed = 2.0;
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut traverser = GraphTraverser::new(
            cycle_graph(),
            &mut speed,
            &mut pos,
            false,
            false,
            PatrolRandom::with_seed(7),
        );
        let mut yaw = 0.0;

        // Ticks 1-13: arrive A, walk to B (2,4,6,8,10), pause, walk to
        // C (12..20), pause.
        let mut xs = Vec::new();
        for _ in 0..13 {
            assert!(traverser.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
            xs.push(pos.x);
        }
        assert_eq!(
            xs,
            vec![
                0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 10.0, 12.0, 14.0, 16.0, 18.0, 20.0, 20.0
            ]
        );

        // Ticks 14-23: the C->A edge wraps the loop and the walk
        // returns to A; tick 24 pauses there, tick 25 sets out for B
        // again — the cycle repeats.
        for _ in 13..23 {
            assert!(traverser.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        }
        assert_eq!(pos.x, 0.0);
        assert!(traverser.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert_eq!(pos.x, 0.0); // the pause tick
        assert!(traverser.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert_eq!(pos.x, 2.0); // B again, loop after wrap
        // The controller never changes the speed without a node range.
        assert_eq!(speed, 2.0);
    }

    /// A positive stay parks the bot for exactly the stay, and the
    /// expiry tick retargets and moves in the same step
    /// (patrol_graph.cpp:406-418).
    #[test]
    fn a_positive_stay_time_parks_the_bot_then_retargtes_in_step() {
        let section = parse(
            "patrol",
            CYCLE_XML
                .replace("<minStay> 0 </minStay>", "<minStay> 2 </minStay>")
                .replace("<maxStay> 0 </maxStay>", "<maxStay> 2 </maxStay>")
                .as_bytes(),
        )
        .unwrap();
        let mut graph = Graph::default();
        assert!(graph.init(&section, 1.0, 1.0));

        let mut speed = 2.0;
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut traverser = GraphTraverser::new(
            Arc::new(graph),
            &mut speed,
            &mut pos,
            false,
            false,
            PatrolRandom::with_seed(7),
        );
        let mut yaw = 0.0;

        // Tick 1: arrival starts the stay. Tick 2: parked (stay 1 left).
        // Tick 3: stay hits 0, retargets to B and moves in the same
        // step.
        assert!(traverser.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert_eq!(pos.x, 0.0);
        assert!(traverser.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert_eq!(pos.x, 0.0);
        assert!(traverser.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert_eq!(pos.x, 2.0);
    }

    /// Node speed ranges redraw the bot's speed: on install (the
    /// constructor draw) and on every retarget (the successor's range);
    /// a node without a range passes the speed through
    /// (patrol_graph.cpp:178-183, 393-394, 416).
    #[test]
    fn node_speed_ranges_redraw_the_bot_speed() {
        let xml = r#"<patrol>
  <nodeDefaults>
    <minSpeed> 3 </minSpeed> <maxSpeed> 3 </maxSpeed>
  </nodeDefaults>
  <nodes>
    <node> <name> A </name> <pos> 0 0 0 </pos>
      <edges> <edge> B </edge> </edges> </node>
    <node> <name> B </name> <pos> 10 0 0 </pos>
      <minSpeed> 4 </minSpeed> <maxSpeed> 4 </maxSpeed>
      <edges> <edge> A </edge> </edges> </node>
  </nodes>
</patrol>"#;
        let section = parse("patrol", xml.as_bytes()).unwrap();
        let mut graph = Graph::default();
        assert!(graph.init(&section, 1.0, 1.0));

        let mut speed = 2.0;
        let mut pos = Vector3::new(0.0, 0.0, 0.0);
        let mut traverser = GraphTraverser::new(
            Arc::new(graph),
            &mut speed,
            &mut pos,
            false,
            false,
            PatrolRandom::with_seed(3),
        );
        let mut yaw = 0.0;
        // The constructor draw landed A's range on the bot's speed.
        assert_eq!(speed, 3.0);

        // Tick 1 parks on A; tick 2 retargets to B and redraws to 4.
        assert!(traverser.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert!(traverser.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        assert_eq!(speed, 4.0);
    }

    /// `scalePos` scales the node positions (and radii), `scaleSpeed`
    /// the speed ranges (patrol_graph.cpp:243-246).
    #[test]
    fn the_scales_stretch_positions_and_speed_ranges() {
        let section = parse("patrol", CYCLE_XML.as_bytes()).unwrap();
        let mut graph = Graph::default();
        assert!(graph.init(&section, 2.0, 0.5));
        assert_eq!(graph.size(), 3);
        assert_eq!(graph.node(1).position(), Vector3::new(20.0, 0.0, 0.0));
        assert_eq!(graph.node(2).position(), Vector3::new(40.0, 0.0, 0.0));

        let xml = r#"<patrol>
  <nodeDefaults> <minSpeed> 3 </minSpeed> <maxSpeed> 3 </maxSpeed> </nodeDefaults>
  <nodes> <node> <name> A </name> <pos> 1 0 0 </pos> </node> </nodes>
</patrol>"#;
        let section = parse("patrol", xml.as_bytes()).unwrap();
        let mut graph = Graph::default();
        assert!(graph.init(&section, 1.0, 2.0));
        assert_eq!(graph.node(0).props().min_speed(), 6.0);
        assert_eq!(graph.node(0).props().max_speed(), 6.0);
        // The position scale leaves the speed range alone and vice
        // versa.
        assert_eq!(graph.node(0).position(), Vector3::new(1.0, 0.0, 0.0));
    }

    /// `Graph::init` refuses a missing `<nodes>` section, duplicate
    /// names and edges naming an unknown node (patrol_graph.cpp:216-269).
    #[test]
    fn graph_init_refuses_broken_documents() {
        let mut graph = Graph::default();
        assert!(!graph.init(
            &parse("patrol", b"<patrol> <nodeDefaults/> </patrol>").unwrap(),
            1.0,
            1.0
        ));

        let dup = r#"<patrol> <nodes>
  <node> <name> A </name> <pos> 0 0 0 </pos> </node>
  <node> <name> A </name> <pos> 1 0 0 </pos> </node>
</nodes> </patrol>"#;
        assert!(!graph.init(&parse("patrol", dup.as_bytes()).unwrap(), 1.0, 1.0));

        let bad_edge = r#"<patrol> <nodes>
  <node> <name> A </name> <pos> 0 0 0 </pos>
    <edges> <edge> Missing </edge> </edges> </node>
</nodes> </patrol>"#;
        assert!(!graph.init(&parse("patrol", bad_edge.as_bytes()).unwrap(), 1.0, 1.0));
    }

    /// A node without an `<edges>` section keeps no adjacencies — the
    /// walk parks on it; an unnamed node takes its index as its name;
    /// node properties fall back to the graph defaults
    /// (patrol_graph.cpp:62-94, 48-57, 189-198).
    #[test]
    fn edgeless_nodes_park_and_unnamed_nodes_take_their_index() {
        let xml = r#"<patrol>
  <nodeDefaults>
    <minStay> 2 </minStay> <maxStay> 5 </maxStay> <radius> 2 </radius>
  </nodeDefaults>
  <nodes> <node> <pos> 3 0 4 </pos> </node> </nodes>
</patrol>"#;
        let section = parse("patrol", xml.as_bytes()).unwrap();
        let mut graph = Graph::default();
        assert!(graph.init(&section, 1.0, 1.0));

        let node = graph.node(0);
        assert_eq!(node.name(), "0");
        assert_eq!(node.props().min_stay(), 2.0);
        assert_eq!(node.props().max_stay(), 5.0);
        assert_eq!(node.props().radius(), 2.0);

        let mut rng = PatrolRandom::with_seed(11);
        assert_eq!(node.next_node(&mut rng), 0);
        let mut pos = Vector3::ZERO;
        assert_eq!(node.randomise_position(&mut pos, &graph, &mut rng), 0);
        assert_eq!(pos, Vector3::new(3.0, 0.0, 4.0));

        // findClosestNode: flat distance, ties keep the earlier node,
        // and an empty graph answers -1.
        let empty = Graph::default();
        assert_eq!(empty.find_closest_node(&Vector3::ZERO), -1);
    }

    /// `findClosestNode` picks the flat-nearest waypoint
    /// (patrol_graph.cpp:275-297).
    #[test]
    fn find_closest_node_picks_the_flat_nearest_waypoint() {
        let graph = cycle_graph();
        assert_eq!(graph.find_closest_node(&Vector3::new(1.0, 500.0, 1.0)), 0);
        assert_eq!(graph.find_closest_node(&Vector3::new(9.0, 0.0, -1.0)), 1);
        assert_eq!(graph.find_closest_node(&Vector3::new(100.0, 0.0, 2.0)), 2);
    }

    /// `randomise` heads for any node (`rand() % size`) instead of the
    /// closest one (patrol_graph.cpp:376-383).
    #[test]
    fn randomise_heads_for_any_node_not_the_closest() {
        let mut mirror = PatrolRandom::with_seed(5);
        let expected = mirror.pick(3);
        let mut speed = 2.0;
        let mut pos = Vector3::new(19.0, 0.0, 0.0); // closest would be C
        let traverser = GraphTraverser::new(
            cycle_graph(),
            &mut speed,
            &mut pos,
            true,
            false,
            PatrolRandom::with_seed(5),
        );
        assert_eq!(traverser.destination_node(), expected);
    }

    /// `snap` lands the start position on the path to a neighbouring
    /// node and heads for that neighbour
    /// (patrol_graph.cpp:387-391, 137-150).
    #[test]
    fn snap_lands_the_start_position_on_an_edge() {
        // A's single adjacency is B, so the pick is a no-op draw and the
        // snap fraction is the stream's next value.
        let mut mirror = PatrolRandom::with_seed(9);
        mirror.pick(1);
        let expected_frac = mirror.c_rand() as f32 / PatrolRandom::RAND_MAX as f32;

        let mut speed = 2.0;
        let mut pos = Vector3::new(0.0, 5.0, 0.0); // closest is A
        let traverser = GraphTraverser::new(
            cycle_graph(),
            &mut speed,
            &mut pos,
            false,
            true,
            PatrolRandom::with_seed(9),
        );
        assert_eq!(traverser.destination_node(), 1); // B

        let dest = Vector3::new(10.0, 0.0, 0.0);
        let expected = Vector3::ZERO + (dest - Vector3::ZERO) * expected_frac;
        assert_eq!(pos, expected);
        // The snapped bot starts on the graph, not where it was.
        assert_eq!(pos.y, 0.0);
    }

    /// The factory flag parse (patrol_graph.cpp:474-503): first field
    /// the graph name, then `random`/`snap`/`scalePos=`/`scaleSpeed=`;
    /// a trailing comma adds no empty field and unknown fields are
    /// swallowed while the parse continues.
    #[test]
    fn factory_args_parse_the_reference_shapes() {
        assert_eq!(
            parse_factory_args("server/bots/test.bwp"),
            ("server/bots/test.bwp".to_string(), false, false, 1.0, 1.0)
        );
        assert_eq!(
            parse_factory_args("g.bwp,random,snap"),
            ("g.bwp".to_string(), true, true, 1.0, 1.0)
        );
        assert_eq!(
            parse_factory_args("g.bwp,scalePos=2.5,scaleSpeed=0.5"),
            ("g.bwp".to_string(), false, false, 2.5, 0.5)
        );
        assert_eq!(
            parse_factory_args("g,bogus,scalePos=3,"),
            ("g".to_string(), false, false, 3.0, 1.0)
        );
        assert_eq!(
            parse_factory_args(""),
            (String::new(), false, false, 1.0, 1.0)
        );
        // A scale that does not parse lands on atof's 0.0.
        assert_eq!(
            parse_factory_args("g,scalePos=abc"),
            ("g".to_string(), false, false, 0.0, 1.0)
        );
    }

    /// The graphs cache (patrol_graph.cpp:324-359): one entry per
    /// (name, scalePos, scaleSpeed) — same scales serve the same graph,
    /// different scales load their own.
    #[test]
    fn graphs_cache_by_decorated_name() {
        let dir = temp_root("cache");
        write(&dir, "/g.bwp", CYCLE_XML);
        let resource = BwResource::init_paths(&[&dir]);

        let mut graphs = Graphs::new();
        let a = graphs.get_graph("g.bwp", 1.0, 1.0, &resource).unwrap();
        let b = graphs.get_graph("g.bwp", 1.0, 1.0, &resource).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        let scaled = graphs.get_graph("g.bwp", 2.0, 1.0, &resource).unwrap();
        assert!(!Arc::ptr_eq(&a, &scaled));
        assert_eq!(scaled.node(1).position(), Vector3::new(20.0, 0.0, 0.0));

        // Unloadable graphs never cache and answer None.
        assert!(
            graphs
                .get_graph("missing.bwp", 1.0, 1.0, &resource)
                .is_none()
        );
        // A loadable document without a <nodes> section inits as false
        // and is dropped the same way (patrol_graph.cpp:349-358).
        write(&dir, "/broken.bwp", "<patrol/>");
        assert!(
            graphs
                .get_graph("broken.bwp", 1.0, 1.0, &resource)
                .is_none()
        );
        // The 1 KiB decorated-name buffer bound.
        let long = format!("{}.bwp", "x".repeat(1000));
        assert!(graphs.get_graph(&long, 1.0, 1.0, &resource).is_none());
    }

    /// The registered factory loads the graph through the resource,
    /// applies the flags and returns a controller that walks the loop —
    /// the fleet wiring face behind `moveType=Patrol`
    /// (main_app.cpp:740-815).
    #[test]
    fn the_registry_creates_a_patrolling_controller() {
        let dir = temp_root("factory");
        write(&dir, "/server/bots/test.bwp", CYCLE_XML);
        let resource = BwResource::init_paths(&[&dir]);

        let mut registry = ControllerRegistry::new();
        registry.register("Patrol", Box::new(PatrolFactory::with_seed(resource, 42)));

        let mut pos = Vector3::ZERO;
        let mut speed = 6.0;
        let mut controller = registry
            .create(
                "Patrol",
                "server/bots/test.bwp,random,snap,scalePos=2,scaleSpeed=1",
                &mut speed,
                &mut pos,
            )
            .unwrap()
            .expect("Patrol creates a controller");
        // scalePos=2 stretched the graph around the snapped start.
        assert!(pos.x >= 0.0 && pos.x <= 40.0);

        let mut yaw = 0.0;
        let start = pos;
        for _ in 0..4 {
            assert!(controller.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
        }
        assert_ne!(pos, start);
    }

    /// A graph that cannot be loaded — or one with an empty `<nodes>`
    /// section, which the reference walks into `node( -1 )` — yields no
    /// controller (patrol_graph.cpp:507-513).
    #[test]
    fn unloadable_or_empty_graphs_create_no_controller() {
        let dir = temp_root("nofile");
        write(&dir, "/empty.bwp", "<patrol> <nodes/> </patrol>");
        let resource = BwResource::init_paths(&[&dir]);

        let mut registry = ControllerRegistry::new();
        registry.register("Patrol", Box::new(PatrolFactory::with_seed(resource, 1)));

        let mut pos = Vector3::ZERO;
        assert!(
            registry
                .create("Patrol", "missing.bwp", &mut 6.0, &mut pos)
                .unwrap()
                .is_none()
        );
        // An empty graph is the reference's node( -1 ) dereference.
        assert!(
            registry
                .create("Patrol", "empty.bwp", &mut 6.0, &mut pos)
                .unwrap()
                .is_none()
        );
        // Empty move data loads no graph either (graph name "").
        assert!(
            registry
                .create("Patrol", "", &mut 6.0, &mut pos)
                .unwrap()
                .is_none()
        );
    }

    /// The seeded stream reproduces the reference's draw faces: `rand()`
    /// stays within RAND_MAX, `unitRand` within [0, 1], `randInRange`
    /// within its bounds, and the same seed repeats the sequence.
    #[test]
    fn the_seeded_stream_reproduces_its_draws() {
        let mut rng = PatrolRandom::with_seed(123);
        let first: Vec<u32> = (0..4).map(|_| rng.c_rand()).collect();
        let mut mirror = PatrolRandom::with_seed(123);
        assert_eq!(first, (0..4).map(|_| mirror.c_rand()).collect::<Vec<_>>());

        let mut rng = PatrolRandom::with_seed(456);
        for _ in 0..100 {
            let r = rng.c_rand();
            assert!(r <= PatrolRandom::RAND_MAX);
            let u = rng.unit();
            assert!((0.0..=1.0).contains(&u));
            let v = rng.rand_in_range(3.0, 5.0);
            assert!((3.0..=5.0).contains(&v));
        }
        // A zero seed still generates (it is remapped).
        let mut rng = PatrolRandom::with_seed(0);
        assert_ne!(rng.c_rand(), rng.c_rand());
    }

    /// `PatrolFactory::new` seeds its stream from the wall clock — the
    /// `srand( timestamp() )` face (main_app.cpp:141) — and serves the
    /// same graphs as a seeded factory.
    #[test]
    fn the_default_factory_seeds_from_the_wall_clock() {
        let dir = temp_root("wallclock");
        write(&dir, "/g.bwp", CYCLE_XML);
        let resource = BwResource::init_paths(&[&dir]);

        let mut registry = ControllerRegistry::new();
        registry.register("Patrol", Box::new(PatrolFactory::new(resource)));

        let mut pos = Vector3::ZERO;
        let mut speed = 2.0;
        let mut controller = registry
            .create("Patrol", "g.bwp", &mut speed, &mut pos)
            .unwrap()
            .expect("the wall-clock factory still creates a controller");

        let mut yaw = 0.0;
        assert!(controller.next_step(&mut speed, 1.0, &mut pos, &mut yaw));
    }
}
