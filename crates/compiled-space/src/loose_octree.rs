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
}
