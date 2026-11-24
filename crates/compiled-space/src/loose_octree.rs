//! BW-10-02-02 static loose octrees: the read side of
//! `lib/math/loose_octree.hpp` / `loose_octree.ipp` as consumed by the
//! static scene provider, plus the static-storage traversal its queries
//! run.
//!
//! The reference reader (`BinaryFormat::Stream::read(StaticLooseOctree&)`,
//! `binary_format.cpp:122-137`) reads one configuration record then five
//! external arrays. The writer side (`prepareStaticTreeStorage`) has
//! depth-first-sorted the nodes and packed every child list, so the static
//! traversal can stop at the first invalid child instead of scanning all
//! eight slots (`loose_octree.ipp:551-566`).

use crate::binary_format::Stream;
use foundation::math::{BoundBox, Vector3};

/// Node indices are `uint16` with the top bit reserved for the leaf flag
/// (`loose_octree.hpp:33-44`).
pub type NodeIndex = u16;
/// Node data references are `uint32`.
pub type NodeDataReference = u32;

pub const INVALID_NODE: NodeIndex = NodeIndex::MAX;
pub const ROOT_NODE: NodeIndex = 0;
pub const INVALID_DATA_REFERENCE: NodeDataReference = u32::MAX;
/// `NodeIndex(~0) ^ (NodeIndex(~0) >> 1)` (`loose_octree.hpp:43`).
pub const LEAF_REF_FLAG: NodeIndex = 0x8000;
pub const INDEX_MASK: NodeIndex = 0x7FFF;
pub const NUM_CHILDREN: usize = 8;

/// `LooseOctree_DataTypes::Configuration`: 24 bytes on the wire
/// (`maxDepth_` u32, center, `worldSize_` f32, `numDataEntries_` u16 plus
/// two padding bytes).
pub const CONFIGURATION_SIZE: usize = 24;
/// `LooseOctree_DataTypes::NodeBoundData` wraps an AABB.
pub const NODE_BOUND_DATA_SIZE: usize = 24;
/// `LooseOctree_DataTypes::NodeCenter` is a Vector3.
pub const NODE_CENTER_SIZE: usize = 12;
/// `LooseOctree_DataTypes::Node` holds eight child references.
pub const NODE_SIZE: usize = 16;

/// `(childReference & LEAF_REF_FLAG) != 0` (`loose_octree.hpp:161-165`).
pub fn is_leaf_ref(child_reference: NodeIndex) -> bool {
    child_reference & LEAF_REF_FLAG != 0
}

/// `childReference & INDEX_MASK` (`loose_octree.hpp:167-172`).
pub fn child_index_from_reference(child_reference: NodeIndex) -> NodeIndex {
    child_reference & INDEX_MASK
}

/// 24-byte configuration record (`loose_octree_DataTypes::Configuration`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Configuration {
    pub max_depth: u32,
    pub world_center: Vector3,
    pub world_size: f32,
    pub num_data_entries: NodeIndex,
}

impl Default for Configuration {
    fn default() -> Self {
        Self::default_config()
    }
}

impl Configuration {
    /// The reference constructor
    /// (`loose_octree.ipp:23-29`): depth 0, zero center, size 1.
    pub const fn default_config() -> Self {
        Self {
            max_depth: 0,
            world_center: Vector3::ZERO,
            world_size: 1.0,
            num_data_entries: 0,
        }
    }

    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != CONFIGURATION_SIZE {
            return None;
        }
        let max_depth = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
        let world_center = Vector3::new(
            f32::from_le_bytes(bytes[4..8].try_into().ok()?),
            f32::from_le_bytes(bytes[8..12].try_into().ok()?),
            f32::from_le_bytes(bytes[12..16].try_into().ok()?),
        );
        let world_size = f32::from_le_bytes(bytes[16..20].try_into().ok()?);
        let num_data_entries = u16::from_le_bytes(bytes[20..22].try_into().ok()?);
        // bytes 22..24 are struct padding and never validated (the
        // reference `read` memcpy's the whole record).
        Some(Self {
            max_depth,
            world_center,
            world_size,
            num_data_entries,
        })
    }
}

/// Eight child references; the static writer packs them and fills the tail
/// with `INVALID_NODE`. The default is the all-invalid node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Node {
    pub child: [NodeIndex; NUM_CHILDREN],
}

impl Default for Node {
    fn default() -> Self {
        Self {
            child: [INVALID_NODE; NUM_CHILDREN],
        }
    }
}

impl Node {
    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        let mut child = [INVALID_NODE; NUM_CHILDREN];
        if bytes.len() != NODE_SIZE {
            return None;
        }
        for (slot, chunk) in child.iter_mut().zip(bytes.as_chunks::<2>().0) {
            *slot = u16::from_le_bytes(*chunk);
        }
        Some(Self { child })
    }
}

/// The static storage of one octree: configuration plus the five arrays the
/// reference `read(StaticLooseOctree&)` populates.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticLooseOctree {
    pub config: Configuration,
    pub bounds: Vec<BoundBox>,
    pub centers: Vec<Vector3>,
    pub nodes: Vec<Node>,
    pub data_references: Vec<NodeDataReference>,
    pub parents: Vec<NodeIndex>,
}

impl StaticLooseOctree {
    /// `BinaryFormat::Stream::read(StaticLooseOctree&)`
    /// (`binary_format.cpp:122-137`): configuration then five external
    /// arrays. Any failed read aborts the whole octree read.
    pub fn read(stream: &mut Stream) -> Option<Self> {
        let config = Configuration::from_le_bytes(stream.read_typed(CONFIGURATION_SIZE)?)?;

        let bounds_raw = stream.read_external_array(NODE_BOUND_DATA_SIZE)?;
        let centers_raw = stream.read_external_array(NODE_CENTER_SIZE)?;
        let nodes_raw = stream.read_external_array(NODE_SIZE)?;
        let data_raw = stream.read_external_array(4)?;
        let parents_raw = stream.read_external_array(2)?;

        // `read_external_array` hands back exactly `count * elem_size`
        // bytes; the chunks divide evenly by construction.
        let bounds = bounds_raw
            .as_chunks::<NODE_BOUND_DATA_SIZE>()
            .0
            .iter()
            .map(|bytes| read_bound_box(bytes))
            .collect();
        let centers = centers_raw
            .as_chunks::<NODE_CENTER_SIZE>()
            .0
            .iter()
            .map(|bytes| read_vector3(bytes))
            .collect();
        let nodes = nodes_raw
            .as_chunks::<NODE_SIZE>()
            .0
            .iter()
            .map(|bytes| Node::from_le_bytes(bytes).expect("chunk is NODE_SIZE"))
            .collect();
        let data_references = data_raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_le_bytes(*bytes))
            .collect();
        let parents = parents_raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| u16::from_le_bytes(*bytes))
            .collect();

        Some(Self {
            config,
            bounds,
            centers,
            nodes,
            data_references,
            parents,
        })
    }

    /// `empty()` is `config_.numDataEntries_ == 0` (`loose_octree.ipp:662-665`).
    pub fn empty(&self) -> bool {
        self.config.num_data_entries == 0
    }

    /// The AABB-shape traversal: `intersect(const AABB&, ...)`
    /// (`loose_octree.ipp:439-460`) driving the generic stack walk
    /// (`loose_octree.ipp:576-628`). Node bounds that miss the query are
    /// skipped; leaves contribute their node index, which the final
    /// `translateDataReferences` (`loose_octree.ipp:158-169`) maps through
    /// `dataReferences_`.
    pub fn intersect_aabb(&self, query: &BoundBox) -> Vec<NodeDataReference> {
        let mut data_references = Vec::new();
        if self.empty() {
            return data_references;
        }

        let mut stack = vec![ROOT_NODE];
        while let Some(node_ref) = stack.pop() {
            let cur_node = child_index_from_reference(node_ref) as usize;
            // `boundOfNode` indexes straight into the array; a malformed
            // file panics here where the reference asserts (release reads
            // out of bounds).
            let node_bound = &self.bounds[cur_node];
            if !query.intersects(node_bound) {
                continue;
            }

            if is_leaf_ref(node_ref) {
                data_references.push(cur_node as NodeDataReference);
            } else {
                let node = &self.nodes[cur_node];
                // Static child lists are packed: stop at the first invalid
                // entry (`loose_octree.ipp:551-566`).
                for child_ref in node.child.iter() {
                    if *child_ref == INVALID_NODE {
                        break;
                    }
                    stack.push(*child_ref);
                }
            }
        }

        // Batch translation of node references into data references.
        for reference in data_references.iter_mut() {
            *reference = self.data_references[*reference as usize];
        }
        data_references
    }
}

/// The dynamic storage of one octree: the same five arrays the reference
/// `LooseOctreeStorage` keeps, but growable — the writer face the static
/// reader came from (`LooseOctree_DynamicTypes`, `loose_octree.hpp:79-90`).
/// Node creation, bound maintenance and traversal are the dynamic half of
/// `LooseOctree<TStorageType>` (`loose_octree.ipp`), as driven by the
/// dynamic scene provider.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DynamicLooseOctree {
    pub config: Configuration,
    pub bounds: Vec<BoundBox>,
    pub centers: Vec<Vector3>,
    pub nodes: Vec<Node>,
    pub data_references: Vec<NodeDataReference>,
    pub parents: Vec<NodeIndex>,
}

impl DynamicLooseOctree {
    /// `LooseOctree::initialise` (`loose_octree.ipp:210-230`): the depth
    /// must be at least one and the storage must be empty; both are
    /// `MF_ASSERT`s in the reference and panic here.
    pub fn initialise(&mut self, center: Vector3, world_size: f32, max_depth: u32) {
        assert!(max_depth >= 1, "reference asserts maxDepth >= 1");
        assert!(self.nodes.is_empty(), "reference asserts no stored data");

        self.config.max_depth = max_depth;
        self.config.world_center = center;
        self.config.world_size = world_size;
        self.config.num_data_entries = 0;

        // Add the root node first
        self.new_node(INVALID_NODE, center, max_depth == 0);
    }

    /// `LooseOctreeStorage::newNode` (`loose_octree.ipp:139-170`): append
    /// to all five arrays; a leaf claims the next data entry. The index
    /// must stay inside `INDEX_MASK` (an `MF_ASSERT` in the reference).
    fn new_node(&mut self, parent: NodeIndex, center: Vector3, is_leaf: bool) -> NodeIndex {
        self.nodes.push(Node::default());
        self.bounds.push(BoundBox::default());
        self.centers.push(center);
        self.parents.push(parent);
        let data_index = if is_leaf {
            self.assign_new_data()
        } else {
            INVALID_DATA_REFERENCE
        };
        self.data_references.push(data_index);

        let new_index = (self.nodes.len() - 1) as NodeIndex;
        assert!(
            (new_index & INDEX_MASK) == new_index,
            "octree node count exceeds the node index capacity"
        );
        new_index
    }

    /// `config_.numDataEntries_++` (`loose_octree.ipp:173-178`).
    fn assign_new_data(&mut self) -> NodeDataReference {
        let entry = self.config.num_data_entries as NodeDataReference;
        self.config.num_data_entries += 1;
        entry
    }

    /// `LooseOctree::reset` (`loose_octree.ipp:244-252`): clears the node
    /// arrays but keeps the configuration.
    pub fn reset(&mut self) {
        self.bounds.clear();
        self.centers.clear();
        self.nodes.clear();
        self.data_references.clear();
        self.parents.clear();
    }

    /// `LooseOctree::reinitialise` (`loose_octree.ipp:233-241`).
    pub fn reinitialise(&mut self) {
        self.reset();
        let center = self.config.world_center;
        let world_size = self.config.world_size;
        let max_depth = self.config.max_depth;
        self.initialise(center, world_size, max_depth);
    }

    /// `LooseOctree::insert(const Vector3&, float)` (`loose_octree.ipp:269-279`):
    /// the bound sphere becomes a box first.
    pub fn insert_sphere(&mut self, obj_center: Vector3, obj_radius: f32) -> NodeIndex {
        let radius_offset = Vector3::new(obj_radius, obj_radius, obj_radius);
        let obj_bb = BoundBox::new(obj_center - radius_offset, obj_center + radius_offset);
        self.insert_bound_box(&obj_bb)
    }

    /// `LooseOctree::insert(const AABB&)` (`loose_octree.ipp:282-297`): an
    /// inside-out box never enters; otherwise the object's box widens the
    /// bound of the leaf its centre falls into.
    pub fn insert_bound_box(&mut self, obj_bb: &BoundBox) -> NodeIndex {
        if obj_bb.inside_out() {
            return INVALID_NODE;
        }

        let cur_node = self.fetch_leaf(obj_bb.centre());
        self.bounds[cur_node as usize].add_bound_box(obj_bb);
        cur_node
    }

    /// `LooseOctree::findData` (`loose_octree.ipp:300-328`): walk without
    /// creating. The reference returns `INVALID_DATAREFERENCE` from this
    /// `NodeIndex` function — the same bit pattern as `INVALID_NODE` after
    /// the u16 truncation, so that is what the port returns.
    pub fn find_data(&self, obj_center: Vector3) -> NodeIndex {
        let mut cur_node = ROOT_NODE;
        while !self.is_leaf(cur_node) {
            let center = self.centers[cur_node as usize];
            let x = if obj_center.x < center.x { 0 } else { 1 };
            let y = if obj_center.y < center.y { 0 } else { 4 };
            let z = if obj_center.z < center.z { 0 } else { 2 };
            let child_ref = self.nodes[cur_node as usize].child[(x | y | z) as usize];
            if child_ref == INVALID_NODE {
                return INVALID_NODE;
            }
            cur_node = child_index_from_reference(child_ref);
        }
        cur_node
    }

    /// `LooseOctree::dataOnLeaf` (`loose_octree.ipp:330-335`).
    pub fn data_on_leaf(&self, node_id: NodeIndex) -> NodeDataReference {
        self.data_references[node_id as usize]
    }

    fn is_leaf(&self, index: NodeIndex) -> bool {
        self.data_references[index as usize] != INVALID_DATA_REFERENCE
    }

    /// `LooseOctree::fetchLeaf` (`loose_octree.ipp:337-389`): descend from
    /// the root creating the missing child chain; children sit a quarter
    /// world-size from their parent's centre along each chosen side, and
    /// the child at `maxDepth` is a leaf.
    fn fetch_leaf(&mut self, obj_center: Vector3) -> NodeIndex {
        let mut cur_node = ROOT_NODE;
        let mut depth: u32 = 0;
        while !self.is_leaf(cur_node) {
            let center = self.centers[cur_node as usize];
            let x = if obj_center.x < center.x { 0 } else { 1 };
            let y = if obj_center.y < center.y { 0 } else { 4 };
            let z = if obj_center.z < center.z { 0 } else { 2 };
            let slot = (x | y | z) as usize;
            let mut child_ref = self.nodes[cur_node as usize].child[slot];

            if child_ref == INVALID_NODE {
                // Create a new node then!
                let half_size = self.config.world_size / (2u32 << depth) as f32;
                let offset = half_size / 2.0;
                let cx = center.x + if x == 0 { -offset } else { offset };
                let cy = center.y + if y == 0 { -offset } else { offset };
                let cz = center.z + if z == 0 { -offset } else { offset };

                let child_depth = depth + 1;
                let is_leaf = child_depth == self.config.max_depth;
                child_ref = self.new_node(cur_node, Vector3::new(cx, cy, cz), is_leaf);
                if is_leaf {
                    child_ref |= LEAF_REF_FLAG;
                }
                // Node is now invalidated! (newNode caused resize)
                self.nodes[cur_node as usize].child[slot] = child_ref;
            }

            depth += 1;
            cur_node = child_index_from_reference(child_ref);
        }
        cur_node
    }

    /// `LooseOctree::mergeChildBounds` (`loose_octree.ipp:674-689`).
    fn merge_child_bounds(&self, index: NodeIndex, bb: &mut BoundBox) {
        let node = &self.nodes[index as usize];
        for child_ref in node.child.iter() {
            if *child_ref != INVALID_NODE {
                let child = child_index_from_reference(*child_ref) as usize;
                bb.add_bound_box(&self.bounds[child]);
            }
        }
    }

    /// `LooseOctree::updateNodeBound(NodeIndex)` (`loose_octree.ipp:743-769`):
    /// recompute every ancestor bound from its children, root bound last.
    pub fn update_node_bound(&mut self, mut node_id: NodeIndex) {
        if node_id == INVALID_NODE {
            return;
        }

        while node_id != ROOT_NODE {
            node_id = self.parents[node_id as usize];

            let mut new_bb = BoundBox::default();
            self.merge_child_bounds(node_id, &mut new_bb);
            self.bounds[node_id as usize] = new_bb;
        }
    }

    /// `LooseOctree::updateNodeBound(NodeIndex, const AABB&)`
    /// (`loose_octree.ipp:772-787`): seed the node with the new bound,
    /// then propagate.
    pub fn update_node_bound_at(&mut self, node_id: NodeIndex, bb: &BoundBox) {
        if node_id == INVALID_NODE {
            return;
        }
        self.bounds[node_id as usize] = *bb;
        self.update_node_bound(node_id);
    }

    /// `LooseOctree::updateHeirarchy()` (`loose_octree.ipp:692-703`): with
    /// only a root node (and any depth above zero) there is nothing to do.
    pub fn update_heirarchy(&mut self) {
        if self.nodes.len() == 1 && self.config.max_depth > 0 {
            // Only a root node exists. Otherwise it is empty
            return;
        }

        self.update_heirarchy_of(ROOT_NODE);
    }

    /// `LooseOctree::updateHeirarchy(NodeIndex)`
    /// (`loose_octree.ipp:706-740`): leaves keep their bound (which must
    /// not be inside-out — an `MF_ASSERT` here), parents merge children.
    fn update_heirarchy_of(&mut self, base_index: NodeIndex) -> BoundBox {
        if self.is_leaf(base_index) {
            let bb = self.bounds[base_index as usize];
            assert!(
                !bb.inside_out(),
                "reference asserts leaf bounds are not inside-out"
            );
            return bb;
        }

        let mut new_bb = BoundBox::default();
        let node = self.nodes[base_index as usize];
        for child_ref in node.child.iter() {
            if *child_ref != INVALID_NODE {
                let child = child_index_from_reference(*child_ref);
                new_bb.add_bound_box(&self.update_heirarchy_of(child));
            }
        }

        self.bounds[base_index as usize] = new_bb;

        // If this BB is inside out then this nodes children are also inside
        // out. Or there are no children - so why is this node even here?
        assert!(
            !new_bb.inside_out(),
            "reference asserts merged bounds are not inside-out"
        );
        new_bb
    }

    /// `empty()` is `config_.numDataEntries_ == 0` (`loose_octree.ipp:668-671`).
    pub fn empty(&self) -> bool {
        self.config.num_data_entries == 0
    }

    /// `rootBoundingBox()` (`loose_octree.ipp:815-819`).
    pub fn root_bound_box(&self) -> &BoundBox {
        &self.bounds[ROOT_NODE as usize]
    }

    /// The AABB-shape traversal for dynamic storage
    /// (`loose_octree.ipp:472-500` + `573-629`): unlike the static walk,
    /// child lists are sparse — all eight slots are scanned with invalid
    /// entries skipped (`pushValidChildren` default, `loose_octree.ipp:541-553`).
    pub fn intersect_aabb(&self, query: &BoundBox) -> Vec<NodeDataReference> {
        let mut data_references = Vec::new();
        if self.empty() {
            return data_references;
        }

        let mut stack = vec![ROOT_NODE];
        while let Some(node_ref) = stack.pop() {
            let cur_node = child_index_from_reference(node_ref) as usize;
            let node_bound = &self.bounds[cur_node];
            if !query.intersects(node_bound) {
                continue;
            }

            if is_leaf_ref(node_ref) {
                data_references.push(cur_node as NodeDataReference);
            } else {
                for child_ref in self.nodes[cur_node].child.iter() {
                    if *child_ref != INVALID_NODE {
                        stack.push(*child_ref);
                    }
                }
            }
        }

        // Batch translation of node references into data references.
        for reference in data_references.iter_mut() {
            *reference = self.data_references[*reference as usize];
        }
        data_references
    }
}

/// Exactly-sized element parsers: the reference memcpy's fixed strides out
/// of the external arrays with no per-element validation, so these index
/// the chunk directly (`binary_format.cpp` `read(ExternalArray<T>&)`).
fn read_bound_box(bytes: &[u8]) -> BoundBox {
    BoundBox::new(
        read_vector3(&bytes[0..NODE_CENTER_SIZE]),
        read_vector3(&bytes[NODE_CENTER_SIZE..]),
    )
}

fn read_vector3(bytes: &[u8]) -> Vector3 {
    Vector3::new(
        f32::from_le_bytes(bytes[0..4].try_into().expect("4 bytes")),
        f32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes")),
        f32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(bytes: &[u8]) -> Stream<'_> {
        Stream::new(bytes)
    }

    fn config_bytes(max_depth: u32, num_data_entries: u16) -> [u8; 24] {
        let mut bytes = [0u8; 24];
        bytes[0..4].copy_from_slice(&max_depth.to_le_bytes());
        bytes[4..8].copy_from_slice(&1.5f32.to_le_bytes());
        bytes[8..12].copy_from_slice(&2.5f32.to_le_bytes());
        bytes[12..16].copy_from_slice(&3.5f32.to_le_bytes());
        bytes[16..20].copy_from_slice(&88.0f32.to_le_bytes());
        bytes[20..22].copy_from_slice(&num_data_entries.to_le_bytes());
        bytes
    }

    fn external_array_bytes(elem_size: usize, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(elem_size as u32).to_le_bytes());
        bytes.extend_from_slice(&((payload.len() / elem_size) as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn node_bytes(child: [NodeIndex; 8]) -> Vec<u8> {
        child.iter().flat_map(|c| c.to_le_bytes()).collect()
    }

    fn bound_box_bytes(min: [f32; 3], max: [f32; 3]) -> Vec<u8> {
        min.iter()
            .chain(max.iter())
            .flat_map(|v| v.to_le_bytes())
            .collect()
    }

    #[test]
    fn leaf_flag_bit_is_the_top_bit() {
        assert_eq!(LEAF_REF_FLAG, 0x8000);
        assert_eq!(INDEX_MASK, 0x7FFF);
        assert!(is_leaf_ref(0x8003));
        assert!(!is_leaf_ref(0x0003));
        assert_eq!(child_index_from_reference(0x8003), 3);
    }

    #[test]
    fn default_config_matches_the_reference_constructor() {
        // `LooseOctree::LooseOctree` (`loose_octree.ipp:23-29`).
        let config = Configuration::default_config();
        assert_eq!(config.max_depth, 0);
        assert_eq!(config.world_center, Vector3::ZERO);
        assert_eq!(config.world_size, 1.0);
        assert_eq!(config.num_data_entries, 0);
        // the derived-in spirit default IS the reference constructor
        assert_eq!(config, Configuration::default());
    }

    #[test]
    fn default_node_is_all_invalid() {
        let node = Node::default();
        assert_eq!(node.child, [INVALID_NODE; NUM_CHILDREN]);
    }

    #[test]
    fn configuration_parses_its_24_bytes() {
        let config = Configuration::from_le_bytes(&config_bytes(2, 5)).unwrap();
        assert_eq!(config.max_depth, 2);
        assert_eq!(config.world_center, Vector3::new(1.5, 2.5, 3.5));
        assert_eq!(config.world_size, 88.0);
        assert_eq!(config.num_data_entries, 5);
    }

    #[test]
    fn configuration_rejects_other_lengths() {
        assert!(Configuration::from_le_bytes(&[0u8; 23]).is_none());
        assert!(Configuration::from_le_bytes(&[0u8; 25]).is_none());
    }

    #[test]
    fn node_parses_eight_child_references() {
        let node = Node::from_le_bytes(&node_bytes([8, 6, 4, 1, 7, 5, 3, 2])).unwrap();
        assert_eq!(node.child, [8, 6, 4, 1, 7, 5, 3, 2]);
    }

    #[test]
    fn node_rejects_other_lengths() {
        assert!(Node::from_le_bytes(&[0u8; 15]).is_none());
        assert!(Node::from_le_bytes(&[0u8; 17]).is_none());
    }

    #[test]
    fn read_consumes_config_then_five_arrays() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(CONFIGURATION_SIZE as u32).to_le_bytes());
        bytes.extend_from_slice(&config_bytes(1, 1));
        bytes.extend(external_array_bytes(
            NODE_BOUND_DATA_SIZE,
            &bound_box_bytes([0.0; 3], [2.0; 3]),
        ));
        bytes.extend(external_array_bytes(
            NODE_CENTER_SIZE,
            &1.0f32.to_le_bytes().repeat(3),
        ));
        bytes.extend(external_array_bytes(
            NODE_SIZE,
            &node_bytes([INVALID_NODE; 8]),
        ));
        bytes.extend(external_array_bytes(4, &7u32.to_le_bytes()));
        bytes.extend(external_array_bytes(2, &9u16.to_le_bytes()));

        let mut stream = stream(&bytes);
        let octree = StaticLooseOctree::read(&mut stream).unwrap();
        assert_eq!(octree.config.max_depth, 1);
        assert_eq!(octree.bounds.len(), 1);
        assert_eq!(octree.centers, vec![Vector3::new(1.0, 1.0, 1.0)]);
        assert_eq!(octree.nodes.len(), 1);
        assert_eq!(octree.data_references, vec![7]);
        assert_eq!(octree.parents, vec![9]);
        assert!(stream.eof());
    }

    #[test]
    fn read_fails_when_any_part_is_missing() {
        // configuration only
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(CONFIGURATION_SIZE as u32).to_le_bytes());
        bytes.extend_from_slice(&config_bytes(1, 0));
        let mut stream = stream(&bytes);
        assert!(StaticLooseOctree::read(&mut stream).is_none());
    }

    #[test]
    fn read_fails_on_a_bad_external_array_element_size() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(CONFIGURATION_SIZE as u32).to_le_bytes());
        bytes.extend_from_slice(&config_bytes(1, 0));
        // node bounds claim 12-byte elements instead of 24
        bytes.extend_from_slice(&12u32.to_le_bytes());
        let mut stream = stream(&bytes);
        assert!(StaticLooseOctree::read(&mut stream).is_none());
    }

    #[test]
    fn empty_octree_yields_no_references() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(CONFIGURATION_SIZE as u32).to_le_bytes());
        bytes.extend_from_slice(&config_bytes(1, 0));
        bytes.extend(external_array_bytes(
            NODE_BOUND_DATA_SIZE,
            &bound_box_bytes([0.0; 3], [2.0; 3]),
        ));
        bytes.extend(external_array_bytes(
            NODE_CENTER_SIZE,
            &1.0f32.to_le_bytes().repeat(3),
        ));
        bytes.extend(external_array_bytes(
            NODE_SIZE,
            &node_bytes([INVALID_NODE; 8]),
        ));
        bytes.extend(external_array_bytes(
            4,
            &INVALID_DATA_REFERENCE.to_le_bytes(),
        ));
        bytes.extend(external_array_bytes(2, &INVALID_NODE.to_le_bytes()));

        let mut stream = stream(&bytes);
        let octree = StaticLooseOctree::read(&mut stream).unwrap();
        assert!(octree.empty());
        assert!(
            octree
                .intersect_aabb(&BoundBox::new(Vector3::ZERO, Vector3::new(2.0, 2.0, 2.0)))
                .is_empty()
        );
    }

    fn two_leaf_octree() -> StaticLooseOctree {
        // root (bounds 0..8) with two packed leaf children holding data
        // references 11 and 22; leaves have bounds 0..4 and 4..8.
        StaticLooseOctree {
            config: Configuration {
                max_depth: 1,
                world_center: Vector3::new(4.0, 4.0, 4.0),
                world_size: 8.0,
                num_data_entries: 2,
            },
            bounds: vec![
                BoundBox::new(Vector3::ZERO, Vector3::new(8.0, 8.0, 8.0)),
                BoundBox::new(Vector3::ZERO, Vector3::new(4.0, 4.0, 4.0)),
                BoundBox::new(Vector3::new(4.0, 4.0, 4.0), Vector3::new(8.0, 8.0, 8.0)),
            ],
            centers: vec![Vector3::new(4.0, 4.0, 4.0); 3],
            nodes: vec![
                Node {
                    child: [
                        LEAF_REF_FLAG | 1,
                        LEAF_REF_FLAG | 2,
                        INVALID_NODE,
                        INVALID_NODE,
                        INVALID_NODE,
                        INVALID_NODE,
                        INVALID_NODE,
                        INVALID_NODE,
                    ],
                },
                Node {
                    child: [INVALID_NODE; 8],
                },
                Node {
                    child: [INVALID_NODE; 8],
                },
            ],
            data_references: vec![INVALID_DATA_REFERENCE, 11, 22],
            parents: vec![INVALID_NODE, 0, 0],
        }
    }

    #[test]
    fn intersect_returns_translated_data_references() {
        let octree = two_leaf_octree();
        let hits =
            octree.intersect_aabb(&BoundBox::new(Vector3::ZERO, Vector3::new(8.0, 8.0, 8.0)));
        assert_eq!(hits, vec![22, 11]);
    }

    #[test]
    fn intersect_skips_nodes_that_miss_the_query() {
        let octree = two_leaf_octree();
        let hits = octree.intersect_aabb(&BoundBox::new(
            Vector3::new(5.0, 5.0, 5.0),
            Vector3::new(6.0, 6.0, 6.0),
        ));
        assert_eq!(hits, vec![22]);
        let hits = octree.intersect_aabb(&BoundBox::new(
            Vector3::new(0.5, 0.5, 0.5),
            Vector3::new(1.0, 1.0, 1.0),
        ));
        assert_eq!(hits, vec![11]);
    }

    #[test]
    fn intersect_on_a_query_missing_every_node_returns_nothing() {
        let octree = two_leaf_octree();
        let hits = octree.intersect_aabb(&BoundBox::new(
            Vector3::new(100.0, 100.0, 100.0),
            Vector3::new(101.0, 101.0, 101.0),
        ));
        assert!(hits.is_empty());
    }

    #[test]
    fn intersect_walks_internal_levels() {
        let mut octree = two_leaf_octree();
        // Make the root's first child an internal node whose own first
        // child is leaf 2; the root's direct leaf 1 child stays second.
        octree.nodes[0].child[0] = 3;
        octree.nodes[0].child[1] = LEAF_REF_FLAG | 1;
        octree.nodes[0].child[2] = LEAF_REF_FLAG | 2;
        let internal = Node {
            child: [
                0x8002,
                INVALID_NODE,
                INVALID_NODE,
                INVALID_NODE,
                INVALID_NODE,
                INVALID_NODE,
                INVALID_NODE,
                INVALID_NODE,
            ],
        };
        octree.nodes.push(internal);
        octree.bounds.push(BoundBox::new(
            Vector3::new(4.0, 4.0, 4.0),
            Vector3::new(8.0, 8.0, 8.0),
        ));
        octree.centers.push(Vector3::new(4.0, 4.0, 4.0));
        octree.data_references.push(INVALID_DATA_REFERENCE);
        octree.parents.push(0);

        let hits = octree.intersect_aabb(&BoundBox::new(
            Vector3::new(4.0, 4.0, 4.0),
            Vector3::new(5.0, 5.0, 5.0),
        ));
        // LIFO order: direct leaf 2, then leaf 1 (the query touches its
        // far corner: closed-interval overlap), then the leaf under the
        // internal node again; each translates through dataReferences_.
        assert_eq!(hits, vec![22, 11, 22]);
    }

    fn dynamic_tree() -> DynamicLooseOctree {
        let mut tree = DynamicLooseOctree::default();
        tree.initialise(Vector3::ZERO, 8.0, 2);
        tree
    }

    fn cube(min: [f32; 3], max: [f32; 3]) -> BoundBox {
        BoundBox::new(
            Vector3::new(min[0], min[1], min[2]),
            Vector3::new(max[0], max[1], max[2]),
        )
    }

    #[test]
    fn dynamic_initialise_adds_one_root() {
        let tree = dynamic_tree();
        assert_eq!(tree.nodes.len(), 1);
        assert!(tree.empty());
        assert_eq!(tree.config.max_depth, 2);
        assert_eq!(tree.config.world_size, 8.0);
        assert_eq!(tree.parents[0], INVALID_NODE);
        assert!(tree.bounds[0].inside_out(), "root starts inside-out");
    }

    #[test]
    #[should_panic(expected = "maxDepth >= 1")]
    fn dynamic_initialise_panics_below_depth_one() {
        let mut tree = DynamicLooseOctree::default();
        tree.initialise(Vector3::ZERO, 8.0, 0);
    }

    #[test]
    #[should_panic(expected = "no stored data")]
    fn dynamic_initialise_panics_on_used_storage() {
        let mut tree = dynamic_tree();
        tree.insert_bound_box(&cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]));
        tree.initialise(Vector3::ZERO, 8.0, 2);
    }

    #[test]
    fn dynamic_insert_rejects_inside_out_boxes() {
        let mut tree = dynamic_tree();
        assert_eq!(tree.insert_bound_box(&BoundBox::default()), INVALID_NODE);
        assert!(tree.empty());
        assert_eq!(tree.nodes.len(), 1);
    }

    #[test]
    fn dynamic_insert_builds_the_reference_child_geometry() {
        let mut tree = dynamic_tree();
        let bb = cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]);
        let leaf = tree.insert_bound_box(&bb);

        // Path from (0,0,0) for centre (5.5, 0.5, 5.5): right|top|back at
        // the root (slot 1|4|2 = 7), then right|bottom|back one level down
        // (slot 1|0|2 = 3).
        let root_child = tree.nodes[0].child[7];
        assert_ne!(root_child, INVALID_NODE);
        assert!(!is_leaf_ref(root_child));
        assert_eq!(
            tree.centers[root_child as usize],
            Vector3::new(2.0, 2.0, 2.0)
        );
        assert_eq!(tree.parents[root_child as usize], 0);

        let leaf_ref = tree.nodes[root_child as usize].child[3];
        assert!(is_leaf_ref(leaf_ref));
        assert_eq!(leaf, child_index_from_reference(leaf_ref));
        assert_eq!(tree.centers[leaf as usize], Vector3::new(3.0, 1.0, 3.0));
        assert_eq!(tree.parents[leaf as usize], root_child);

        assert_eq!(tree.data_on_leaf(leaf), 0);
        assert!(!tree.empty());
        assert_eq!(tree.bounds[leaf as usize], bb);
    }

    #[test]
    fn dynamic_second_leaf_gets_the_next_data_entry() {
        let mut tree = dynamic_tree();
        let east = tree.insert_bound_box(&cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]));
        let west = tree.insert_bound_box(&cube([-6.0, 0.0, -6.0], [-5.0, 1.0, -5.0]));
        assert_ne!(east, west);
        assert_eq!(tree.data_on_leaf(west), 1);
        assert_eq!(tree.config.num_data_entries, 2);
    }

    #[test]
    fn dynamic_insert_sphere_widens_by_the_radius_cube() {
        let mut tree = dynamic_tree();
        let leaf = tree.insert_sphere(Vector3::new(1.0, 2.0, 3.0), 0.5);
        assert_eq!(
            tree.bounds[leaf as usize],
            cube([0.5, 1.5, 2.5], [1.5, 2.5, 3.5])
        );
    }

    #[test]
    fn dynamic_update_node_bound_repairs_the_ancestor_chain() {
        let mut tree = dynamic_tree();
        let bb = cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]);
        let leaf = tree.insert_bound_box(&bb);
        // `insert` itself only widens the leaf; the provider repairs the
        // chain explicitly.
        tree.update_node_bound(leaf);
        let root_child = child_index_from_reference(tree.nodes[0].child[7]);
        assert_eq!(tree.bounds[root_child as usize], bb);
        assert_eq!(tree.bounds[0], bb);
        assert_eq!(*tree.root_bound_box(), bb);
    }

    #[test]
    fn dynamic_update_node_bound_at_seeds_then_propagates() {
        let mut tree = dynamic_tree();
        let leaf = tree.insert_bound_box(&cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]));
        let grown = cube([5.0, 0.0, 5.0], [7.0, 1.0, 6.0]);
        tree.update_node_bound_at(leaf, &grown);
        assert_eq!(tree.bounds[leaf as usize], grown);
        assert_eq!(tree.bounds[0], grown);
        // the INVALID_NODE overloads are no-ops
        tree.update_node_bound(INVALID_NODE);
        tree.update_node_bound_at(INVALID_NODE, &grown);
        assert_eq!(tree.bounds[0], grown);
    }

    #[test]
    fn dynamic_update_heirarchy_merges_both_children() {
        let mut tree = dynamic_tree();
        tree.insert_bound_box(&cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]));
        tree.insert_bound_box(&cube([-6.0, 0.0, -6.0], [-5.0, 1.0, -5.0]));
        tree.update_heirarchy();
        assert_eq!(tree.bounds[0], cube([-6.0, 0.0, -6.0], [6.0, 1.0, 6.0]));
    }

    #[test]
    fn dynamic_update_heirarchy_skips_a_lone_root() {
        let mut tree = dynamic_tree();
        // No children exist: the early-out leaves the inside-out root
        // bound untouched instead of asserting.
        tree.update_heirarchy();
        assert!(tree.bounds[0].inside_out());
    }

    #[test]
    fn dynamic_intersect_translates_leaf_data_references() {
        let mut tree = dynamic_tree();
        tree.insert_bound_box(&cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]));
        tree.insert_bound_box(&cube([-6.0, 0.0, -6.0], [-5.0, 1.0, -5.0]));
        tree.update_heirarchy();

        let whole = tree.intersect_aabb(&cube([-8.0, -8.0, -8.0], [8.0, 8.0, 8.0]));
        let mut whole = whole;
        whole.sort_unstable();
        assert_eq!(whole, vec![0, 1]);

        let east = tree.intersect_aabb(&cube([4.0, -1.0, 4.0], [7.0, 2.0, 7.0]));
        assert_eq!(east, vec![0]);

        let empty = tree.intersect_aabb(&cube([100.0, 100.0, 100.0], [101.0, 101.0, 101.0]));
        assert!(empty.is_empty());
    }

    #[test]
    fn dynamic_find_data_walks_without_creating() {
        let tree = dynamic_tree();
        assert_eq!(tree.find_data(Vector3::new(1.0, 1.0, 1.0)), INVALID_NODE);
        assert_eq!(tree.nodes.len(), 1, "no nodes were created");

        let mut tree = dynamic_tree();
        let leaf = tree.insert_bound_box(&cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]));
        assert_eq!(
            tree.find_data(cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]).centre()),
            leaf
        );
    }

    #[test]
    fn dynamic_reinitialise_keeps_the_configuration() {
        let mut tree = dynamic_tree();
        tree.insert_bound_box(&cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]));
        tree.reinitialise();
        assert_eq!(tree.nodes.len(), 1);
        assert!(tree.empty());
        assert_eq!(tree.config.max_depth, 2);
        assert_eq!(tree.config.world_size, 8.0);
        assert_eq!(tree.config.world_center, Vector3::ZERO);
    }

    #[test]
    fn dynamic_reset_clears_arrays_but_keeps_config() {
        let mut tree = dynamic_tree();
        tree.insert_bound_box(&cube([5.0, 0.0, 5.0], [6.0, 1.0, 6.0]));
        tree.reset();
        assert!(tree.nodes.is_empty());
        assert!(tree.bounds.is_empty());
        assert_eq!(tree.config.max_depth, 2);
    }
}
