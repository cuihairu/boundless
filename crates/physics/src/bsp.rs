//! BW-10-03-04: the collision BSP tree (`lib/physics2/bsp.cpp`).
//!
//! `BSPTreeTool::loadBSP` (`bsp.cpp:1360-1384`) dispatches on the encoded
//! magic word to the current format (version 2, `loadBSPCurrent`
//! `bsp.cpp:1393-1510`) or the legacy format (version 0, `loadBSPLegacy`
//! `bsp.cpp:1512-1604`). The current format is an in-place layout of
//! triangles, shared-triangle offsets and nodes (`saveBSPInMemory`
//! `bsp.cpp:1197+` writes it); the legacy format is rebuilt into the
//! current shape through the constructor export (`exportAsTree`
//! `bsp.cpp:612-802`). Both paths end in `BSPTree::validate()`
//! (`bsp.cpp:1035-1064`, active — `BSP_RUNTIME_VALIDATION` is defined at
//! `bsp.cpp:23`).

use std::fmt;

use foundation::math::{BoundBox, PlaneEq, Vector3};

use crate::world_triangle::{TRIANGLE_NOT_IN_BSP, WORLD_TRIANGLE_SIZE, WorldTriangle};

/// `BSP_FILE_MAGIC` — "BSP" (`bsp.cpp:1027`).
pub const BSP_FILE_MAGIC: u32 = 0x0050_5342;
/// `BSP_FILE_LEGACY_VERSION` (`bsp.cpp:1023`).
pub const BSP_FILE_LEGACY_VERSION: u8 = 0;
/// `BSP_FILE_CUR_VERSION` (`bsp.cpp:1025`).
pub const BSP_FILE_CUR_VERSION: u8 = 2;
/// `BSP_FILE_INVALID_VERSION` (`bsp.cpp:1026`).
pub const BSP_FILE_INVALID_VERSION: u8 = 0xff;
/// `BSP_FILE_LEGACY_TOKEN` = magic with the version in the top byte.
pub const BSP_FILE_LEGACY_TOKEN: u32 = BSP_FILE_MAGIC | ((BSP_FILE_LEGACY_VERSION as u32) << 24);
/// `BSP_FILE_CUR_TOKEN`.
pub const BSP_FILE_CUR_TOKEN: u32 = BSP_FILE_MAGIC | ((BSP_FILE_CUR_VERSION as u32) << 24);

/// `sizeof(PlaneEq)` — 16 bytes, checked by the header.
pub const PLANE_EQ_SIZE: u32 = 16;
/// `BSPNode::PACKED_SIZE` — 40 bytes (`bsp.cpp:75-77`).
pub const BSP_NODE_PACKED_SIZE: u32 = 40;
/// `OFFSET_32BIT_NULL` (`bsp.cpp:26`): null encoded as -1 element index.
pub const OFFSET_32BIT_NULL: i32 = -1;

/// The load errors. The reference reports these through `ERROR_MSG` and
/// returns NULL (`bsp.cpp:1374-1382, 1424-1430`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BspLoadError {
    /// `BSPTree::load: Bad bsp magic` — the INVALID_VERSION switch case.
    BadMagic,
    /// `BSPTree::load: Unsupported bsp version. Expected %d or %d. Got %u.`
    /// Unreachable through `load` (see `bsp_version`), kept for the
    /// reference's switch shape.
    UnsupportedVersion { got: u8 },
    /// `Failed to load bsp due to base structure sizes mismatch`. The
    /// reference MF_ASSERTs the same condition first (assert builds abort
    /// there); the port surfaces the mismatch as this error — the
    /// reference's own graceful branch.
    SizeMismatch,
    /// Registered divergence: the reference reads out of bounds where the
    /// port reports truncation.
    Truncated,
}

impl fmt::Display for BspLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BspLoadError::BadMagic => f.write_str("BSPTree::load: Bad bsp magic"),
            BspLoadError::UnsupportedVersion { got } => write!(
                f,
                "BSPTree::load: Unsupported bsp version. Expected {} or {}. Got {}.",
                BSP_FILE_LEGACY_VERSION, BSP_FILE_CUR_VERSION, got
            ),
            BspLoadError::SizeMismatch => {
                f.write_str("Failed to load bsp due to base structure sizes mismatch")
            }
            BspLoadError::Truncated => {
                f.write_str("Registered divergence: the reference reads out of bounds here.")
            }
        }
    }
}

impl std::error::Error for BspLoadError {}

/// `BSPTreeTool::getBSPVersion` (`bsp.cpp:1329-1353`). Note the masked
/// generic-version branch can never fire: the mask keeps the version byte
/// and strips the magic's low byte (0x42), so the comparison can never
/// hold for any input. Ported as written.
// The dead masked branch is ported as written; clippy's bad_bit_mask
// flags exactly the unreachability the doc comment registers.
#[allow(clippy::bad_bit_mask)]
pub fn bsp_version(data: &[u8]) -> u8 {
    // MF_ASSERT(len >= sizeof(uint32)).
    assert!(data.len() >= 4, "reference MF_ASSERTs a four-byte block");
    let encoded = u32::from_le_bytes(data[0..4].try_into().unwrap());
    match encoded {
        BSP_FILE_CUR_TOKEN => BSP_FILE_CUR_VERSION,
        BSP_FILE_LEGACY_TOKEN => BSP_FILE_LEGACY_VERSION,
        _ => {
            if (encoded & 0xffff_ff00) == BSP_FILE_MAGIC {
                // Dead branch in the reference (see the doc comment); the
                // uint8 return truncates the 24-bit value.
                (encoded & 0x00ff_ffff) as u8
            } else {
                BSP_FILE_INVALID_VERSION
            }
        }
    }
}

/// One node of the tree. Offsets are element indices into the tree's
/// arrays (`offsetToPtr`/`ptrToOffset`, `bsp.cpp:1083-1106`); null is
/// `OFFSET_32BIT_NULL`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BspNode {
    pub front: Option<usize>,
    pub back: Option<usize>,
    pub plane_eq: PlaneEq,
    /// First of this node's run of triangles.
    pub triangles: Option<usize>,
    pub num_triangles: u32,
    /// First of this node's run of shared-triangle indices.
    pub shared: Option<usize>,
    pub num_shared: u32,
}

/// A loaded collision BSP tree.
#[derive(Debug, Clone, PartialEq)]
pub struct BspTree {
    nodes: Vec<BspNode>,
    triangles: Vec<WorldTriangle>,
    /// Indices into `triangles`; nodes' shared runs slice this.
    shared_ptrs: Vec<usize>,
    bounding_box: BoundBox,
    user_data: Vec<(u32, Vec<u8>)>,
}

/// The reference sort comparator (`WorldTriangleRef::operator<`,
/// `bsp.cpp:458-469`): lexicographic, and a strict prefix compares equal
/// (returns false both ways). A stable sort with it reproduces the
/// reference's libstdc++ insertion sort at fixture scale (registered:
/// larger inputs go through the reference's unstable introsort).
fn compare_node_lists(a: &[usize], b: &[usize]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let len = a.len().min(b.len());
    for i in 0..len {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

impl BspTree {
    /// `BSPTreeTool::loadBSP` (`bsp.cpp:1360-1384`).
    pub fn load(data: &[u8]) -> Result<Self, BspLoadError> {
        match bsp_version(data) {
            BSP_FILE_CUR_VERSION => Self::load_current(data),
            BSP_FILE_LEGACY_VERSION => Self::load_legacy(data),
            BSP_FILE_INVALID_VERSION => Err(BspLoadError::BadMagic),
            got => Err(BspLoadError::UnsupportedVersion { got }),
        }
    }

    /// `loadBSPCurrent` (`bsp.cpp:1393-1510`).
    fn load_current(data: &[u8]) -> Result<Self, BspLoadError> {
        let need = |n: usize, at: usize| -> Result<(), BspLoadError> {
            if at + n > data.len() {
                Err(BspLoadError::Truncated)
            } else {
                Ok(())
            }
        };
        // Header: magic (already dispatched on), then the three structure
        // sizes the format relies on.
        need(16, 0)?;
        let word = |i: usize| u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap());
        let plane_size = word(1);
        let triangle_size = word(2);
        let node_packed_size = word(3);
        // The reference MF_ASSERTs these and then re-checks with
        // ERROR_MSG + NULL; the port keeps the graceful branch only (see
        // the SizeMismatch doc comment).
        if plane_size != PLANE_EQ_SIZE
            || triangle_size != WORLD_TRIANGLE_SIZE as u32
            || node_packed_size != BSP_NODE_PACKED_SIZE
        {
            return Err(BspLoadError::SizeMismatch);
        }

        let num_triangles = word(4) as usize;
        let num_nodes = word(5) as usize;
        let num_shared = word(6) as usize;

        // Precalculated bounding box: `sizeof(BoundingBox)` = 32 bytes —
        // the AABB min/max (24) plus two `Outcode` render words the save
        // round-trips (`boundbox.hpp:74-91`, `mathdef.hpp:93`
        // Outcode = uint32; save `bsp.cpp:1240`, load `bsp.cpp:1418`).
        // The port keeps min/max and drops the outcodes. The readPODs here
        // have no bounds assert in the reference — the port reports
        // truncation (registered divergence) where the reference would
        // read OOB.
        need(60 + num_triangles * WORLD_TRIANGLE_SIZE, 0)?;
        let vf = |i: usize| {
            Vector3::new(
                f32::from_le_bytes(data[i..i + 4].try_into().unwrap()),
                f32::from_le_bytes(data[i + 4..i + 8].try_into().unwrap()),
                f32::from_le_bytes(data[i + 8..i + 12].try_into().unwrap()),
            )
        };
        let bounding_box = BoundBox::new(vf(28), vf(40));

        let mut at = 60;
        let mut triangles = Vec::with_capacity(num_triangles);
        for _ in 0..num_triangles {
            triangles.push(WorldTriangle::from_row(&data[at..at + WORLD_TRIANGLE_SIZE]));
            at += WORLD_TRIANGLE_SIZE;
        }

        // Shared triangles: element offsets into the triangle array.
        // The reference asserts the whole array fits (`bsp.cpp:1467`).
        assert!(
            at + num_shared * 4 <= data.len(),
            "reference MF_ASSERTs the shared offsets fit in the block"
        );
        let mut shared_ptrs = Vec::with_capacity(num_shared);
        for _ in 0..num_shared {
            let off = i32::from_le_bytes(data[at..at + 4].try_into().unwrap());
            // A null here would hand the reference a NULL triangle
            // pointer that faults on use; the port refuses at load.
            shared_ptrs.push(decode_offset(off).expect("reference stores a null shared pointer"));
            at += 4;
        }

        // Nodes: the reference asserts both that there is at least one and
        // that they all fit (`bsp.cpp:1481-1486`).
        assert!(num_nodes > 0, "reference MF_ASSERTs at least one node");
        assert!(
            at + num_nodes * BSP_NODE_PACKED_SIZE as usize <= data.len(),
            "reference MF_ASSERTs the nodes fit in the block"
        );
        let mut nodes = Vec::with_capacity(num_nodes);
        for _ in 0..num_nodes {
            let i32_at =
                |o: usize| i32::from_le_bytes(data[at + o..at + o + 4].try_into().unwrap());
            let u32_at =
                |o: usize| u32::from_le_bytes(data[at + o..at + o + 4].try_into().unwrap());
            nodes.push(BspNode {
                front: decode_offset(i32_at(0)),
                back: decode_offset(i32_at(4)),
                plane_eq: PlaneEq::new(
                    vf(at + 8),
                    f32::from_le_bytes(data[at + 20..at + 24].try_into().unwrap()),
                ),
                triangles: decode_offset(i32_at(24)),
                num_triangles: u32_at(28),
                shared: decode_offset(i32_at(32)),
                num_shared: u32_at(36),
            });
            at += BSP_NODE_PACKED_SIZE as usize;
        }

        // User data: repeated (key u32, len i32, blob) over the tail
        // (`loadUserData` `bsp.cpp:1612-1631`). The reference reads the
        // loop blindly (OOB on ragged or overlong tails — registered
        // divergence; the port reports truncation instead) and asserts
        // exact tiling at the end, which the need() guards subsume.
        let mut user_data = Vec::new();
        while at < data.len() {
            need(8, at)?;
            let key = u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
            let len = i32::from_le_bytes(data[at + 4..at + 8].try_into().unwrap());
            at += 8;
            // Negative lengths crash the reference allocation; the port
            // panics the same way.
            assert!(len >= 0, "reference allocation aborts on negative length");
            need(len as usize, at)?;
            user_data.push((key, data[at..at + len as usize].to_vec()));
            at += len as usize;
        }

        let tree = Self {
            nodes,
            triangles,
            shared_ptrs,
            bounding_box,
            user_data,
        };
        tree.validate();
        Ok(tree)
    }

    /// `loadBSPLegacy` (`bsp.cpp:1512-1604`): the 1.x layout (per-node
    /// u16 triangle-index lists, back links found by skipping the front
    /// subtree), converted to the current shape by the constructor export
    /// (`exportAsTree` `bsp.cpp:612-802`).
    fn load_legacy(data: &[u8]) -> Result<Self, BspLoadError> {
        const BSP_HAS_FRONT: u8 = 0x02;
        const BSP_HAS_BACK: u8 = 0x04;

        let need = |n: usize, at: usize| -> Result<(), BspLoadError> {
            if at + n > data.len() {
                Err(BspLoadError::Truncated)
            } else {
                Ok(())
            }
        };
        // magic, numTriangles, numNodes, maxTriangles (skipped).
        need(16, 0)?;
        let word = |i: usize| u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap());
        let num_triangles = word(1) as usize;
        let num_nodes = word(2) as usize;

        need(16 + num_triangles * WORLD_TRIANGLE_SIZE, 0)?;
        let mut triangles = Vec::with_capacity(num_triangles);
        for i in 0..num_triangles {
            triangles.push(WorldTriangle::from_row(
                &data[16 + i * WORLD_TRIANGLE_SIZE..16 + (i + 1) * WORLD_TRIANGLE_SIZE],
            ));
        }
        let mut at = 16 + num_triangles * WORLD_TRIANGLE_SIZE;

        // Node records: flags u8, PlaneEq 16, numRefTris u16, then the
        // u16 triangle indices. The back child of a both-children node is
        // found by skipping the whole front subtree (`bsp.cpp:1560-1591`).
        // The walk reads with no bounds checks in the reference — the
        // port reports truncation (registered divergence).
        struct RawNode {
            front: Option<usize>,
            back: Option<usize>,
            plane: PlaneEq,
            tri_indices: Vec<u16>,
        }
        let vf = |base: usize| {
            Vector3::new(
                f32::from_le_bytes(data[base..base + 4].try_into().unwrap()),
                f32::from_le_bytes(data[base + 4..base + 8].try_into().unwrap()),
                f32::from_le_bytes(data[base + 8..base + 12].try_into().unwrap()),
            )
        };
        let mut raw_nodes = Vec::with_capacity(num_nodes);
        for i in 0..num_nodes {
            need(19, at)?;
            let node_start = at;
            let flags = data[at];
            let plane = PlaneEq::new(
                vf(at + 1),
                f32::from_le_bytes(data[at + 13..at + 17].try_into().unwrap()),
            );
            at += 17;
            let mut front = None;
            let mut back = None;
            if flags & BSP_HAS_FRONT != 0 {
                front = Some(i + 1);
                if flags & BSP_HAS_BACK != 0 {
                    let mut depth = 1;
                    let mut num_skipped = 0;
                    let mut nodemem = node_start;
                    while depth > 0 {
                        // Skip this node's record, then peek the flags of
                        // the node that follows it.
                        nodemem += 1 + 16;
                        need(2, nodemem)?;
                        let num_tris =
                            u16::from_le_bytes(data[nodemem..nodemem + 2].try_into().unwrap());
                        nodemem += 2 + num_tris as usize * 2;
                        need(1, nodemem)?;
                        let nflags = data[nodemem];
                        if nflags & BSP_HAS_FRONT != 0 {
                            depth += 1;
                        }
                        if nflags & BSP_HAS_BACK != 0 {
                            depth += 1;
                        }
                        depth -= 1;
                        num_skipped += 1;
                    }
                    back = Some(i + num_skipped + 1);
                }
            } else if flags & BSP_HAS_BACK != 0 {
                back = Some(i + 1);
            }
            let num_ref_tris = u16::from_le_bytes(data[at..at + 2].try_into().unwrap()) as usize;
            at += 2;
            need(num_ref_tris * 2, at)?;
            let mut tri_indices = Vec::with_capacity(num_ref_tris);
            for _ in 0..num_ref_tris {
                let idx = u16::from_le_bytes(data[at..at + 2].try_into().unwrap());
                assert!(
                    (idx as usize) < num_triangles,
                    "reference MF_ASSERTs the triangle index is in range"
                );
                tri_indices.push(idx);
                at += 2;
            }
            raw_nodes.push(RawNode {
                front,
                back,
                plane,
                tri_indices,
            });
        }

        // --- exportAsTree (`bsp.cpp:612-802`) -------------------------
        // Which nodes reference each triangle, in node order.
        let mut tri_refs: Vec<Vec<usize>> = vec![Vec::new(); num_triangles];
        for (i, n) in raw_nodes.iter().enumerate() {
            for &t in &n.tri_indices {
                tri_refs[t as usize].push(i);
            }
        }
        // MF_ASSERT(each triangle is referenced by at least one node).
        for refs in &tri_refs {
            assert!(
                !refs.is_empty(),
                "reference MF_ASSERTs every triangle has a referee"
            );
        }

        // Sort by node list with the reference comparator (prefix ==
        // equal), keeping the stable scan order within ties.
        let mut order: Vec<usize> = (0..num_triangles).collect();
        order.sort_by(|&a, &b| compare_node_lists(&tri_refs[a], &tri_refs[b]));
        let sorted_triangles: Vec<WorldTriangle> = order.iter().map(|&i| triangles[i]).collect();

        // Assign each run's owning node, count the shared references.
        let mut num_triangles_of = vec![0u32; num_nodes];
        let mut run_start = vec![None; num_nodes];
        let mut num_shared_of = vec![0u32; num_nodes];
        let mut last_node: Option<usize> = None;
        let mut run_len = 0u32;
        for (pos, &tri) in order.iter().enumerate() {
            let node0 = tri_refs[tri][0];
            if Some(node0) != last_node {
                if let Some(prev) = last_node {
                    num_triangles_of[prev] = run_len;
                }
                run_start[node0] = Some(pos);
                run_len = 0;
                last_node = Some(node0);
            }
            for &other in &tri_refs[tri][1..] {
                num_shared_of[other] += 1;
            }
            run_len += 1;
        }
        if let Some(prev) = last_node {
            num_triangles_of[prev] = run_len;
        }
        // (The reference's else arm zeroes node 0 for the empty case; the
        // port's defaults are already that.)

        // Shared runs are laid out in node order (the reference MF_ASSERTs
        // its cursor lands exactly on the end — the port's exact-size
        // extend guarantees it), then filled in referee order.
        let mut shared_layout = vec![None; num_nodes];
        let mut shared_ptrs = Vec::new();
        for (i, layout) in shared_layout.iter_mut().enumerate() {
            if num_shared_of[i] > 0 {
                *layout = Some(shared_ptrs.len());
                shared_ptrs.extend(std::iter::repeat_n(0usize, num_shared_of[i] as usize));
            }
        }
        let mut filled = vec![0u32; num_nodes];
        for (pos, &tri) in order.iter().enumerate() {
            for &other in &tri_refs[tri][1..] {
                let base = shared_layout[other].expect("layout exists for every referee");
                shared_ptrs[base + filled[other] as usize] = pos;
                filled[other] += 1;
            }
        }

        let nodes: Vec<BspNode> = raw_nodes
            .iter()
            .enumerate()
            .map(|(i, n)| BspNode {
                front: n.front,
                back: n.back,
                plane_eq: n.plane,
                triangles: run_start[i],
                num_triangles: num_triangles_of[i],
                shared: shared_layout[i],
                num_shared: num_shared_of[i],
            })
            .collect();

        // User data — the same tail format, loaded by the same reference
        // function as the current loader.
        let mut user_data = Vec::new();
        while at < data.len() {
            need(8, at)?;
            let key = u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
            let len = i32::from_le_bytes(data[at + 4..at + 8].try_into().unwrap());
            at += 8;
            assert!(len >= 0, "reference allocation aborts on negative length");
            need(len as usize, at)?;
            user_data.push((key, data[at..at + len as usize].to_vec()));
            at += len as usize;
        }

        let tree = Self {
            nodes,
            bounding_box: Self::generate_bounding_box(&sorted_triangles),
            triangles: sorted_triangles,
            shared_ptrs,
            user_data,
        };
        tree.validate();
        Ok(tree)
    }

    /// `generateBoundingBox`: inside-out box with every triangle vertex
    /// added.
    fn generate_bounding_box(triangles: &[WorldTriangle]) -> BoundBox {
        let mut bb = BoundBox::default();
        for tri in triangles {
            bb.add_bounds(&tri.v0());
            bb.add_bounds(&tri.v1());
            bb.add_bounds(&tri.v2());
        }
        bb
    }

    /// `BSPTree::validate` (`bsp.cpp:1035-1064`, active): structural
    /// sanity. Every MF_ASSERT is a panic here.
    fn validate(&self) {
        // MF_ASSERT(pNodes_ == pRoot_): the root is node 0 by
        // construction in the port.
        let mut num_node_links = 1u32;
        let mut num_shared_tris = 0u32;
        for n in &self.nodes {
            assert!(
                (n.triangles.is_none() && n.num_triangles == 0)
                    || (n.triangles.is_some() && n.num_triangles > 0),
                "reference MF_ASSERTs a node's triangle run is empty or non-empty"
            );
            assert!(
                (n.shared.is_none() && n.num_shared == 0)
                    || (n.shared.is_some() && n.num_shared > 0),
                "reference MF_ASSERTs a node's shared run is empty or non-empty"
            );
            if let Some(start) = n.triangles {
                assert!(
                    start + n.num_triangles as usize <= self.triangles.len(),
                    "reference MF_ASSERTs the triangle run is in bounds"
                );
            }
            if let Some(start) = n.shared {
                assert!(
                    start + n.num_shared as usize <= self.shared_ptrs.len(),
                    "reference MF_ASSERTs the shared run is in bounds"
                );
            }
            for child in [n.front, n.back].into_iter().flatten() {
                num_node_links += 1;
                assert!(
                    child < self.nodes.len(),
                    "reference MF_ASSERTs the child offset is in bounds"
                );
            }
            num_shared_tris += n.num_shared;
        }
        assert!(
            self.nodes.len() as u32 == num_node_links,
            "reference MF_ASSERTs every non-root node is somebody's child"
        );
        assert!(
            self.shared_ptrs.len() as u32 == num_shared_tris,
            "reference MF_ASSERTs the shared array matches the node counts"
        );
    }

    /// `triangles()` (`bsp.ipp`).
    pub fn triangles(&self) -> &[WorldTriangle] {
        &self.triangles
    }

    /// `numTriangles()`.
    pub fn num_triangles(&self) -> u32 {
        self.triangles.len() as u32
    }

    /// `numNodes()`.
    pub fn num_nodes(&self) -> u32 {
        self.nodes.len() as u32
    }

    /// `empty()` — no triangles (`bsp.ipp`).
    pub fn empty(&self) -> bool {
        self.triangles.is_empty()
    }

    /// `boundingBox()` (`bsp.ipp`).
    pub fn bounding_box(&self) -> &BoundBox {
        &self.bounding_box
    }

    pub fn nodes(&self) -> &[BspNode] {
        &self.nodes
    }

    /// The root node is the first node (`pRoot_ = pNodes_`).
    pub fn root(&self) -> &BspNode {
        &self.nodes[0]
    }

    /// A node's own triangles (its run into the triangle array).
    pub fn node_triangles(&self, node: &BspNode) -> &[WorldTriangle] {
        match node.triangles {
            Some(start) => &self.triangles[start..start + node.num_triangles as usize],
            None => &[],
        }
    }

    /// A node's shared triangles (its run into the shared-pointer array).
    pub fn node_shared_triangles(&self, node: &BspNode) -> impl Iterator<Item = &WorldTriangle> {
        let (start, len) = match node.shared {
            Some(start) => (start, node.num_shared as usize),
            None => (0, 0),
        };
        self.shared_ptrs[start..start + len]
            .iter()
            .map(move |&i| &self.triangles[i])
    }

    /// The user data blobs keyed like `getUserData`.
    pub fn user_data(&self, key: u32) -> Option<&[u8]> {
        self.user_data
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_slice())
    }

    /// `canCollide`: true when any triangle's collision flags differ from
    /// `TRIANGLE_NOT_IN_BSP`.
    pub fn can_collide(&self) -> bool {
        self.triangles
            .iter()
            .any(|t| t.collision_flags() != TRIANGLE_NOT_IN_BSP)
    }

    /// `BSPNode::intersectsThisNode(start, end, dist, ppHitTriangle,
    /// pVisitor)` (`bsp.cpp:221-247`): scan the node's own run then its
    /// shared run, skipping `TRIANGLE_NOT_IN_BSP` candidates. Every hit
    /// narrows `dist`; a rejected visit restores the previous value. The
    /// node's scan does NOT stop at the first hit — the last accepted
    /// (i.e. closest) candidate wins.
    fn intersects_this_node<'a>(
        &'a self,
        node: &BspNode,
        start: Vector3,
        direction_vec: Vector3,
        dist: &mut f32,
        mut visitor: Option<&mut (dyn CollisionVisitor + '_)>,
        hit: &mut Option<&'a WorldTriangle>,
    ) -> bool {
        let mut b_intersects = false;

        for candidate in self
            .node_triangles(node)
            .iter()
            .chain(self.node_shared_triangles(node))
        {
            if candidate.collision_flags() != TRIANGLE_NOT_IN_BSP {
                let original_dist = *dist;

                let accepted = candidate.intersects(start, direction_vec, dist)
                    && match visitor.as_deref_mut() {
                        None => true,
                        Some(v) => v.visit(candidate, *dist),
                    };

                if accepted {
                    b_intersects = true;
                    *hit = Some(candidate);
                } else {
                    *dist = original_dist;
                }
            }
        }

        b_intersects
    }

    /// `BSPTree::intersects(start, end, dist, ppHitTriangle, pVisitor)`
    /// (`bsp.cpp:1865-1997`): the iterative near-to-far BSP walk with a
    /// `BSPStackNode` stack (`DynamicEmbeddedArray<BSPStackNode, 32>`).
    /// `dist` in is the fraction of the interval to consider (usually 1);
    /// on a hit it is the fraction the collision occurred at, so the hit
    /// point is `start + dist * (end - start)`. Returns the hit triangle
    /// (the reference's `ppHitTriangle` out-param; callers that pass NULL
    /// ignore it — no in-tree caller seeds it, so the port has no
    /// initial-hit parameter; registered).
    pub fn intersects(
        &self,
        start: Vector3,
        end: Vector3,
        dist: &mut f32,
        visitor: Option<&mut dyn CollisionVisitor>,
    ) -> Option<&WorldTriangle> {
        struct StackNode<'a> {
            node: &'a BspNode,
            /// -1 for unseen, -2 marks nothing (leaves set both sides
            /// before the branch below).
            e_back: i32,
            s_dist: f32,
            e_dist: f32,
        }

        let mut visitor = visitor;
        let orig_dist = *dist;
        let mut hit: Option<&WorldTriangle> = None;
        let orig_hit = hit;

        let delta = end - start;
        let tolerance_pct = BSPNODE_TOLERANCE / delta.length();

        let mut stack: Vec<StackNode> = vec![StackNode {
            node: self.root(),
            e_back: -1,
            s_dist: 0.0,
            e_dist: 1.0,
        }];

        while let Some(cur) = stack.pop() {
            // Set default / initial values for the line segment range.
            let s_dist = cur.s_dist;
            let e_dist = cur.e_dist;

            // Set up for plane intersection.
            let mut i_dist = 0.0;
            let pe = &cur.node.plane_eq;

            // Variables saying if points are on the back side of the
            // plane.
            let s_back;
            let e_back;

            // See if this is a really simple node.
            if cur.node.front.is_none() && cur.node.back.is_none() {
                // Just look at the triangles and don't try to add to the
                // stack. (The reference's sBack is never read on this
                // path, so the port does not bind it.)
                e_back = -2;
            }
            // See if this is the first time we've seen it.
            else if cur.e_back == -1 {
                let s_out = pe.distance_to(&(start + delta * (s_dist - tolerance_pct)));
                let e_out = pe.distance_to(&(start + delta * (e_dist + tolerance_pct)));
                s_back = (s_out < 0.0) as i32;
                e_back = (e_out < 0.0) as i32;

                // Find which side the start is on (`(&pFront_)[sBack]`:
                // index 0 is front, 1 is back).
                let start_side = if s_back == 0 {
                    cur.node.front
                } else {
                    cur.node.back
                };

                // Are they both on the same side?
                if s_back == e_back {
                    // But first check if either is within tolerance: some
                    // triangles might slightly cross the partition plane.
                    if s_out.abs() < BSPNODE_TOLERANCE || e_out.abs() < BSPNODE_TOLERANCE {
                        stack.push(StackNode {
                            node: cur.node,
                            e_back: 1 - s_back,
                            s_dist,
                            e_dist,
                        });
                    }

                    // Now go down the start side.
                    if let Some(side) = start_side {
                        stack.push(StackNode {
                            node: &self.nodes[side],
                            e_back: -1,
                            s_dist,
                            e_dist,
                        });
                    }

                    continue;
                }

                // The points are on different sides: find the intersect
                // distance.
                i_dist = pe.intersect_ray_half(&start, pe.normal().dot_product(&delta));

                // If there's anything on the start side we'll have to do
                // it first.
                if let Some(side) = start_side {
                    // Remember to come to the end side.
                    stack.push(StackNode {
                        node: cur.node,
                        e_back,
                        s_dist,
                        e_dist,
                    });

                    // And then look at the start side.
                    stack.push(StackNode {
                        node: &self.nodes[side],
                        e_back: -1,
                        s_dist,
                        e_dist: i_dist,
                    });

                    continue;
                }

                // Nothing on the start side: fall through to check this
                // node's triangles and later add the end side.
            }
            // We've been here before.
            else {
                // (The reference's sBack assignment here is dead — only
                // eBack is read below.)
                e_back = cur.e_back;

                i_dist = pe.intersect_ray_half(&start, pe.normal().dot_product(&delta));

                // Fall through to check this node's triangles and later
                // add the end side.
            }

            // Check the triangles on this node, and get out immediately
            // if we get a (confirmed) hit.
            if self.intersects_this_node(
                cur.node,
                start,
                delta,
                dist,
                visitor.as_deref_mut(),
                &mut hit,
            ) && *dist <= (e_dist + tolerance_pct)
            {
                return hit;
            }

            // Reset stuff.
            *dist = orig_dist;
            hit = orig_hit;

            // Now add the other side if it's not null (and eBack is ok).
            if e_back >= 0 {
                let end_side = if e_back == 0 {
                    cur.node.front
                } else {
                    cur.node.back
                };
                if let Some(side) = end_side {
                    stack.push(StackNode {
                        node: &self.nodes[side],
                        e_back: -1,
                        s_dist: i_dist,
                        e_dist,
                    });
                }
            }
        }

        None
    }

    /// `BSPNode::intersectsThisNode(triangle, ppHitTriangle)`
    /// (`bsp.cpp:191-220`): static overlap scan of the node's own run
    /// then shared run, skipping `TRIANGLE_NOT_IN_BSP` candidates. The
    /// reference writes the INPUT triangle (never the candidate) to
    /// `*ppHitTriangle`, so the boolean carries all the information — the
    /// port returns it directly.
    fn intersects_this_node_triangle(&self, node: &BspNode, triangle: &WorldTriangle) -> bool {
        for candidate in self
            .node_triangles(node)
            .iter()
            .chain(self.node_shared_triangles(node))
        {
            if candidate.collision_flags() != TRIANGLE_NOT_IN_BSP
                && candidate.intersects_triangle(triangle)
            {
                return true;
            }
        }
        false
    }

    /// `BSPNode::intersectsThisNode(triangle, translation, pVisitor)`
    /// (`bsp.cpp:164-190`): translated-prism scan of the node's runs. The
    /// visitor is consulted with the candidate and a `0.0` dist; a
    /// rejected visit keeps scanning (no dist is kept or restored on this
    /// face).
    fn intersects_this_node_sweep(
        &self,
        node: &BspNode,
        triangle: &WorldTriangle,
        translation: Vector3,
        mut visitor: Option<&mut (dyn CollisionVisitor + '_)>,
    ) -> bool {
        for candidate in self
            .node_triangles(node)
            .iter()
            .chain(self.node_shared_triangles(node))
        {
            if candidate.collision_flags() != TRIANGLE_NOT_IN_BSP
                && candidate.intersects_swept(triangle, translation)
                && match visitor.as_deref_mut() {
                    None => true,
                    Some(v) => v.visit(candidate, 0.0),
                }
            {
                return true;
            }
        }
        false
    }

    /// `BSPNode::intersects(triangle, ppHitTriangle)` (`bsp.cpp:308-365`):
    /// the recursive overlap walk; the side holding the FIRST vertex
    /// (`d0`) is descended first, the node's own run is scanned only when
    /// the triangle straddles the partition plane within tolerance.
    fn intersects_triangle_node(&self, node: &BspNode, triangle: &WorldTriangle) -> bool {
        if node.front.is_none() && node.back.is_none() {
            return self.intersects_this_node_triangle(node, triangle);
        }

        let d0 = node.plane_eq.distance_to(&triangle.v0());
        let d1 = node.plane_eq.distance_to(&triangle.v1());
        let d2 = node.plane_eq.distance_to(&triangle.v2());

        let min = min3(d0, d1, d2);
        let max = max3(d0, d1, d2);

        // Check the side with the first point of the triangle first.
        if d0 < 0.0 {
            if let Some(back) = node.back
                && min < BSPNODE_TOLERANCE
                && self.intersects_triangle_node(&self.nodes[back], triangle)
            {
                return true;
            }

            if min < BSPNODE_TOLERANCE
                && max > -BSPNODE_TOLERANCE
                && self.intersects_this_node_triangle(node, triangle)
            {
                return true;
            }

            if let Some(front) = node.front
                && max > -BSPNODE_TOLERANCE
                && self.intersects_triangle_node(&self.nodes[front], triangle)
            {
                return true;
            }
        } else {
            if let Some(front) = node.front
                && max > -BSPNODE_TOLERANCE
                && self.intersects_triangle_node(&self.nodes[front], triangle)
            {
                return true;
            }

            if min < BSPNODE_TOLERANCE
                && max > -BSPNODE_TOLERANCE
                && self.intersects_this_node_triangle(node, triangle)
            {
                return true;
            }

            if let Some(back) = node.back
                && min < BSPNODE_TOLERANCE
                && self.intersects_triangle_node(&self.nodes[back], triangle)
            {
                return true;
            }
        }

        false
    }

    /// `BSPNode::intersects(triangle, translation, pVisitor)`
    /// (`bsp.cpp:372-425`): the prism walk — the swept volume crossed
    /// with the node's plane decides which subtrees are entered (own run
    /// first, then back, then front).
    fn intersects_sweep_node(
        &self,
        node: &BspNode,
        triangle: &WorldTriangle,
        translation: Vector3,
        visitor: Option<&mut (dyn CollisionVisitor + '_)>,
    ) -> bool {
        // If we're not partitioned it's easy.
        if node.front.is_none() && node.back.is_none() {
            return self.intersects_this_node_sweep(node, triangle, translation, visitor);
        }

        // See if the volume crosses this plane.
        let d_a0 = node.plane_eq.distance_to(&triangle.v0());
        let d_a1 = node.plane_eq.distance_to(&triangle.v1());
        let d_a2 = node.plane_eq.distance_to(&triangle.v2());
        let d_b0 = node.plane_eq.distance_to(&(triangle.v0() + translation));
        let d_b1 = node.plane_eq.distance_to(&(triangle.v1() + translation));
        let d_b2 = node.plane_eq.distance_to(&(triangle.v2() + translation));

        let min_a = min3(d_a0, d_a1, d_a2);
        let min_b = min3(d_b0, d_b1, d_b2);
        let max_a = max3(d_a0, d_a1, d_a2);
        let max_b = max3(d_b0, d_b1, d_b2);
        // std::min/std::max: the first argument wins ties.
        let min = if min_b < min_a { min_b } else { min_a };
        let max = if max_b > max_a { max_b } else { max_a };

        let mut visitor = visitor;

        if min < BSPNODE_TOLERANCE
            && max > -BSPNODE_TOLERANCE
            && self.intersects_this_node_sweep(node, triangle, translation, visitor.as_deref_mut())
        {
            return true;
        }

        if let Some(back) = node.back
            && min < BSPNODE_TOLERANCE
            && self.intersects_sweep_node(
                &self.nodes[back],
                triangle,
                translation,
                visitor.as_deref_mut(),
            )
        {
            return true;
        }

        if let Some(front) = node.front
            && max > -BSPNODE_TOLERANCE
            && self.intersects_sweep_node(&self.nodes[front], triangle, translation, visitor)
        {
            return true;
        }

        false
    }

    /// `BSPTree::intersects(triangle, ppHitTriangle)`
    /// (`bsp.cpp:1811-1814`): whether the input triangle overlaps any
    /// triangle assigned to the tree.
    pub fn intersects_triangle(&self, triangle: &WorldTriangle) -> bool {
        self.intersects_triangle_node(self.root(), triangle)
    }

    /// `BSPTree::intersects(triangle, translation, pVisitor)`
    /// (`bsp.cpp:2013-2016`): whether the prism swept by `triangle` along
    /// `translation` intersects any triangle in the tree.
    pub fn intersects_sweep(
        &self,
        triangle: &WorldTriangle,
        translation: Vector3,
        visitor: Option<&mut dyn CollisionVisitor>,
    ) -> bool {
        self.intersects_sweep_node(self.root(), triangle, translation, visitor)
    }
}

/// `BSPNODE_TOLERANCE` (`bsp.cpp:85`).
pub const BSPNODE_TOLERANCE: f32 = 0.01;

/// `min3` (`bsp.cpp:283-291`): the reference ternary chain, kept verbatim.
fn min3(a: f32, b: f32, c: f32) -> f32 {
    if a < b {
        if a < c { a } else { c }
    } else {
        if b < c { b } else { c }
    }
}

/// `max3` (`bsp.cpp:293-299`): the reference ternary chain, kept verbatim.
fn max3(a: f32, b: f32, c: f32) -> f32 {
    if a > b {
        if a > c { a } else { c }
    } else {
        if b > c { b } else { c }
    }
}

/// `CollisionVisitor` (`bsp.hpp:22-31`): receives every accepted hit
/// triangle in a node scan; returning false rejects the hit (the walk
/// restores the previous `dist` and keeps scanning).
pub trait CollisionVisitor {
    fn visit(&mut self, hit_triangle: &WorldTriangle, dist: f32) -> bool;
}

/// `offsetToPtr` (`bsp.cpp:1083-1092`): null for `OFFSET_32BIT_NULL`,
/// `MF_ASSERT(offset >= 0)` otherwise.
fn decode_offset(off: i32) -> Option<usize> {
    if off == OFFSET_32BIT_NULL {
        None
    } else {
        assert!(off >= 0, "reference MF_ASSERTs the offset is not negative");
        Some(off as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_triangle::TRIANGLE_TERRAIN;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    fn v(x: f32, y: f32, z: f32) -> Vector3 {
        Vector3::new(x, y, z)
    }

    fn words(vals: &[u32]) -> Vec<u8> {
        vals.iter().flat_map(|w| w.to_le_bytes()).collect()
    }

    fn v3_bytes(v: Vector3) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&v.x.to_le_bytes());
        b.extend_from_slice(&v.y.to_le_bytes());
        b.extend_from_slice(&v.z.to_le_bytes());
        b
    }

    fn tri_bytes(v0: Vector3, v1: Vector3, v2: Vector3, flags: u16) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(&v3_bytes(v0));
        b.extend(&v3_bytes(v1));
        b.extend(&v3_bytes(v2));
        b.extend_from_slice(&flags.to_le_bytes());
        b.extend_from_slice(&[0u8; 2]);
        b
    }

    #[allow(clippy::too_many_arguments)]
    fn node_bytes(
        front: i32,
        back: i32,
        normal: Vector3,
        d: f32,
        tris: i32,
        num_tris: u32,
        shared: i32,
        num_shared: u32,
    ) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&front.to_le_bytes());
        b.extend_from_slice(&back.to_le_bytes());
        b.extend(&v3_bytes(normal));
        b.extend_from_slice(&d.to_le_bytes());
        b.extend_from_slice(&tris.to_le_bytes());
        b.extend_from_slice(&num_tris.to_le_bytes());
        b.extend_from_slice(&shared.to_le_bytes());
        b.extend_from_slice(&num_shared.to_le_bytes());
        assert_eq!(b.len(), BSP_NODE_PACKED_SIZE as usize);
        b
    }

    fn user_bytes(key: u32, data: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&key.to_le_bytes());
        b.extend_from_slice(&(data.len() as u32).to_le_bytes());
        b.extend_from_slice(data);
        b
    }

    /// A current-format tree: root splits at x=0 owning t0 and t1, the
    /// front leaf shares t1, the back leaf owns t2 (flags low byte 0xff).
    fn current_blob(tri_flags: [u16; 3]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(&words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 3, 3, 1]));
        b.extend(&v3_bytes(v(-1.0, -1.0, 0.0)));
        b.extend(&v3_bytes(v(1.0, 1.0, 2.0)));
        // The box section is sizeof(BoundingBox) = 32: min, max and two
        // zero Outcode words, exactly what saveBSPInMemory round-trips.
        b.extend(&[0u8; 8]);
        b.extend(&tri_bytes(
            v(-1.0, -1.0, 0.0),
            v(1.0, -1.0, 0.0),
            v(0.0, 1.0, 0.0),
            tri_flags[0],
        ));
        b.extend(&tri_bytes(
            v(-1.0, -1.0, 1.0),
            v(1.0, -1.0, 1.0),
            v(0.0, 1.0, 1.0),
            tri_flags[1],
        ));
        b.extend(&tri_bytes(
            v(-1.0, -1.0, 2.0),
            v(1.0, -1.0, 2.0),
            v(0.0, 1.0, 2.0),
            tri_flags[2],
        ));
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend(&node_bytes(
            1,
            2,
            v(1.0, 0.0, 0.0),
            0.0,
            0,
            2,
            OFFSET_32BIT_NULL,
            0,
        ));
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 1.0, 0.0),
            5.0,
            OFFSET_32BIT_NULL,
            0,
            0,
            1,
        ));
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 1.0, 0.0),
            6.0,
            2,
            1,
            OFFSET_32BIT_NULL,
            0,
        ));
        b.extend(&user_bytes(7, b"coal"));
        b.extend(&user_bytes(9, b"w"));
        b
    }

    fn legacy_node(flags: u8, normal: Vector3, d: f32, refs: &[u16]) -> Vec<u8> {
        let mut b = Vec::new();
        b.push(flags);
        b.extend(&v3_bytes(normal));
        b.extend_from_slice(&d.to_le_bytes());
        b.extend_from_slice(&(refs.len() as u16).to_le_bytes());
        for r in refs {
            b.extend_from_slice(&r.to_le_bytes());
        }
        b
    }

    fn legacy_blob(nodes: &[Vec<u8>], tris: &[Vec<u8>], user: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(&words(&[
            BSP_FILE_LEGACY_TOKEN,
            tris.len() as u32,
            nodes.len() as u32,
            0,
        ]));
        for t in tris {
            b.extend(t);
        }
        for n in nodes {
            b.extend(n);
        }
        b.extend(user);
        b
    }

    fn flat(tri: &[Vec<u8>]) -> Vec<Vec<u8>> {
        tri.to_vec()
    }

    #[test]
    fn version_tokens_dispatch_like_the_reference() {
        let cur = words(&[BSP_FILE_CUR_TOKEN]);
        let legacy = words(&[BSP_FILE_LEGACY_TOKEN]);
        assert_eq!(bsp_version(&cur), BSP_FILE_CUR_VERSION);
        assert_eq!(bsp_version(&legacy), BSP_FILE_LEGACY_VERSION);
        // Garbage and a foreign version byte both land in INVALID: the
        // masked branch is dead because the mask strips the magic's low
        // byte, which is 0x42.
        assert_eq!(bsp_version(&[1, 2, 3, 4]), BSP_FILE_INVALID_VERSION);
        assert_eq!(
            bsp_version(&words(&[BSP_FILE_MAGIC | (1 << 24)])),
            BSP_FILE_INVALID_VERSION
        );
        // The dead branch's body, exercised for the port's own shape.
        let encoded = BSP_FILE_MAGIC & 0xffff_ff00; // what the mask would compare
        assert_eq!(encoded & 0x00ff_ffff, 0x0050_5300);
    }

    #[test]
    fn short_input_asserts_like_the_reference_mf_assert() {
        let r = catch_unwind(AssertUnwindSafe(|| bsp_version(&[1, 2, 3])));
        assert!(r.is_err(), "reference MF_ASSERTs a four-byte block");
    }

    #[test]
    fn current_format_round_trips_the_full_layout() {
        let blob = current_blob([0, 1 << 3, 0x00ff]);
        let tree = BspTree::load(&blob).expect("the fixture loads");

        assert_eq!(tree.num_triangles(), 3);
        assert_eq!(tree.num_nodes(), 3);
        assert!(!tree.empty());

        let t0 = WorldTriangle::new(v(-1.0, -1.0, 0.0), v(1.0, -1.0, 0.0), v(0.0, 1.0, 0.0), 0);
        let t1 = WorldTriangle::new(
            v(-1.0, -1.0, 1.0),
            v(1.0, -1.0, 1.0),
            v(0.0, 1.0, 1.0),
            1 << 3,
        );
        let t2 = WorldTriangle::new(
            v(-1.0, -1.0, 2.0),
            v(1.0, -1.0, 2.0),
            v(0.0, 1.0, 2.0),
            0x00ff,
        );
        assert_eq!(tree.triangles(), &[t0, t1, t2]);

        let root = tree.root();
        assert_eq!(root.front, Some(1));
        assert_eq!(root.back, Some(2));
        assert_eq!(root.plane_eq, PlaneEq::new(v(1.0, 0.0, 0.0), 0.0));
        assert_eq!(tree.node_triangles(root), &[t0, t1]);
        assert_eq!(root.shared, None);

        let front = &tree.nodes()[1];
        assert_eq!(front.triangles, None);
        let shared: Vec<_> = tree.node_shared_triangles(front).collect();
        assert_eq!(shared, vec![&t1]);

        let back = &tree.nodes()[2];
        assert_eq!(tree.node_triangles(back), &[t2]);
        assert_eq!(tree.node_shared_triangles(back).count(), 0);

        assert_eq!(tree.bounding_box().min, v(-1.0, -1.0, 0.0));
        assert_eq!(tree.bounding_box().max, v(1.0, 1.0, 2.0));

        // Flags split: t2 is NOT_IN_BSP with material kind 0, t1 carries
        // the terrain collision bit.
        assert_eq!(tree.triangles()[2].collision_flags(), TRIANGLE_NOT_IN_BSP);
        assert_eq!(tree.triangles()[1].collision_flags(), TRIANGLE_TERRAIN);
        assert!(tree.can_collide());

        assert_eq!(tree.user_data(7), Some(&b"coal"[..]));
        assert_eq!(tree.user_data(9), Some(&b"w"[..]));
        assert_eq!(tree.user_data(1), None);
    }

    #[test]
    fn all_not_in_bsp_trees_cannot_collide() {
        let tree = BspTree::load(&current_blob([0x00ff; 3])).expect("loads");
        assert!(!tree.can_collide());
    }

    #[test]
    fn empty_trees_load_and_report_empty() {
        // One leaf node, no triangles, no shared: the reference's validate
        // accepts it and empty() holds.
        let mut b = Vec::new();
        b.extend(&words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 0, 1, 0]));
        b.extend(&v3_bytes(v(0.0, 0.0, 0.0)));
        b.extend(&v3_bytes(v(0.0, 0.0, 0.0)));
        b.extend(&[0u8; 8]); // the two Outcode words of sizeof(BoundingBox)
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 0.0, 1.0),
            0.0,
            OFFSET_32BIT_NULL,
            0,
            OFFSET_32BIT_NULL,
            0,
        ));
        let tree = BspTree::load(&b).expect("loads");
        assert!(tree.empty());
        assert!(!tree.can_collide());
        // The current format keeps the stored box as-is (no
        // regeneration on load).
        assert_eq!(
            tree.bounding_box(),
            &BoundBox::new(v(0.0, 0.0, 0.0), v(0.0, 0.0, 0.0))
        );
    }

    #[test]
    fn size_mismatch_reports_the_reference_error() {
        let mut b = words(&[BSP_FILE_CUR_TOKEN, 15, 40, 40, 0, 1, 0]);
        b.extend(&[0u8; 24]);
        let err = BspTree::load(&b).expect_err("sizes are checked");
        assert_eq!(err, BspLoadError::SizeMismatch);
        assert_eq!(
            err.to_string(),
            "Failed to load bsp due to base structure sizes mismatch"
        );
    }

    #[test]
    fn bad_magic_reports_the_reference_error() {
        let err = BspTree::load(&[9, 9, 9, 9, 0, 0, 0, 0]).expect_err("magic is checked");
        assert_eq!(err, BspLoadError::BadMagic);
        assert_eq!(err.to_string(), "BSPTree::load: Bad bsp magic");
        // The truncation error's message documents the registered
        // divergence (the reference reads out of bounds instead).
        assert_eq!(
            BspLoadError::Truncated.to_string(),
            "Registered divergence: the reference reads out of bounds here."
        );
        // The unsupported-version arm keeps the reference message shape.
        let err = BspLoadError::UnsupportedVersion { got: 1 };
        assert_eq!(
            err.to_string(),
            "BSPTree::load: Unsupported bsp version. Expected 0 or 2. Got 1."
        );
    }

    #[test]
    fn truncation_is_reported_where_the_reference_reads_oob() {
        let blob = current_blob([0, 0, 0]);
        // Ragged user-data tail.
        assert_eq!(
            BspTree::load(&blob[..blob.len() - 1]),
            Err(BspLoadError::Truncated)
        );
        // Mid bounding box.
        assert_eq!(BspTree::load(&blob[..30]), Err(BspLoadError::Truncated));
        // Mid triangle (60 header/count/bbox bytes + 52).
        assert_eq!(BspTree::load(&blob[..112]), Err(BspLoadError::Truncated));
        // Stray bytes after well-formed user data.
        let mut stray = blob.clone();
        stray.extend_from_slice(&[0xaa, 0xbb, 0xcc]);
        assert_eq!(
            BspTree::load(&stray),
            Err(BspLoadError::Truncated),
            "{}",
            BspLoadError::Truncated
        );
        // Legacy: mid node record.
        let legacy = legacy_blob(
            &[legacy_node(0x02, v(0.0, 1.0, 0.0), 4.0, &[0])],
            &flat(&[tri_bytes(
                v(0.0, 0.0, 0.0),
                v(1.0, 0.0, 0.0),
                v(0.0, 1.0, 0.0),
                0,
            )]),
            &[],
        );
        assert_eq!(BspTree::load(&legacy[..20]), Err(BspLoadError::Truncated));
    }

    #[test]
    fn shortfalls_assert_like_the_reference() {
        // No nodes at all.
        let mut b = words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 0, 0, 0]);
        b.extend(&[0u8; 32]);
        assert!(catch_unwind(AssertUnwindSafe(|| BspTree::load(&b))).is_err());

        // Shared offsets claim three but only two fit (the array-fit
        // MF_ASSERT fires before any node is read).
        let mut b = words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 0, 1, 3]);
        b.extend(&[0u8; 32]);
        b.extend(&[0u8; 8]);
        assert!(catch_unwind(AssertUnwindSafe(|| BspTree::load(&b))).is_err());

        // Nodes claim two but only one fits.
        let mut b = words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 0, 2, 0]);
        b.extend(&[0u8; 32]);
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 0.0, 1.0),
            0.0,
            OFFSET_32BIT_NULL,
            0,
            OFFSET_32BIT_NULL,
            0,
        ));
        assert!(catch_unwind(AssertUnwindSafe(|| BspTree::load(&b))).is_err());

        // A negative, non-null offset in the real shared slot
        // (MF_ASSERT(offset >= 0)).
        let mut b = words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 0, 1, 1]);
        b.extend(&[0u8; 32]);
        b.extend(&(-2i32).to_le_bytes());
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 0.0, 1.0),
            0.0,
            OFFSET_32BIT_NULL,
            0,
            OFFSET_32BIT_NULL,
            0,
        ));
        assert!(catch_unwind(AssertUnwindSafe(|| BspTree::load(&b))).is_err());

        // A negative user-data length.
        let mut b = words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 0, 1, 0]);
        b.extend(&[0u8; 32]);
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 0.0, 1.0),
            0.0,
            OFFSET_32BIT_NULL,
            0,
            OFFSET_32BIT_NULL,
            0,
        ));
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&(-1i32).to_le_bytes());
        assert!(catch_unwind(AssertUnwindSafe(|| BspTree::load(&b))).is_err());
    }

    #[test]
    fn validate_catches_a_dangling_child_offset() {
        let mut b = Vec::new();
        b.extend(&words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 1, 2, 0]));
        b.extend(&v3_bytes(v(0.0, 0.0, 0.0)));
        b.extend(&v3_bytes(v(1.0, 1.0, 1.0)));
        b.extend(&[0u8; 8]); // the two Outcode words of sizeof(BoundingBox)
        b.extend(&tri_bytes(
            v(0.0, 0.0, 0.0),
            v(1.0, 0.0, 0.0),
            v(0.0, 1.0, 0.0),
            0,
        ));
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            99,
            v(1.0, 0.0, 0.0),
            0.0,
            0,
            1,
            OFFSET_32BIT_NULL,
            0,
        ));
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 1.0, 0.0),
            0.0,
            OFFSET_32BIT_NULL,
            0,
            OFFSET_32BIT_NULL,
            0,
        ));
        let r = catch_unwind(AssertUnwindSafe(|| BspTree::load(&b)));
        assert!(r.is_err(), "validate rejects the out-of-range child");
    }

    #[test]
    fn legacy_format_converts_like_the_constructor_export() {
        // t0 is referenced by nodes [0, 1], t1 by [0]. The reference
        // comparator treats [0,1] vs [0] as equal (prefix), so the scan
        // order survives: t0 first, then t1.
        let t0 = tri_bytes(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 1);
        let t1 = tri_bytes(v(2.0, 0.0, 0.0), v(3.0, 0.0, 0.0), v(2.0, 1.0, 0.0), 2);
        let blob = legacy_blob(
            &[
                legacy_node(0x02 | 0x04, v(1.0, 0.0, 0.0), 0.0, &[0, 1]),
                legacy_node(0, v(0.0, 1.0, 0.0), 5.0, &[0]),
                legacy_node(0, v(0.0, 1.0, 0.0), 6.0, &[]),
            ],
            &flat(&[t0, t1]),
            &user_bytes(3, b"tea"),
        );
        let tree = BspTree::load(&blob).expect("the legacy fixture loads");

        assert_eq!(tree.num_triangles(), 2);
        assert_eq!(tree.num_nodes(), 3);
        // Scan order preserved: t0 (flags 1) then t1 (flags 2).
        assert_eq!(tree.triangles()[0].flags(), 1);
        assert_eq!(tree.triangles()[1].flags(), 2);

        let root = tree.root();
        assert_eq!(root.front, Some(1));
        assert_eq!(root.back, Some(2));
        assert_eq!(root.triangles, Some(0));
        assert_eq!(root.num_triangles, 2);
        assert_eq!(root.plane_eq, PlaneEq::new(v(1.0, 0.0, 0.0), 0.0));

        let front = &tree.nodes()[1];
        assert_eq!(front.triangles, None);
        assert_eq!(front.shared, Some(0));
        assert_eq!(front.num_shared, 1);
        let shared: Vec<_> = tree.node_shared_triangles(front).collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].flags(), 1, "the shared slot holds t0, not t1");

        let back = &tree.nodes()[2];
        assert_eq!(back.triangles, None);
        assert_eq!(back.shared, None);
        assert_eq!(back.num_shared, 0);

        // The legacy tree regenerates its box from the vertices.
        assert_eq!(tree.bounding_box().min, v(0.0, 0.0, 0.0));
        assert_eq!(tree.bounding_box().max, v(3.0, 1.0, 0.0));
        assert!(tree.can_collide());
        assert_eq!(tree.user_data(3), Some(&b"tea"[..]));
    }

    #[test]
    fn legacy_walk_resolves_deeply_nested_back_children() {
        // root(0) has front 1 and back 4; node 1 has front 2 and back 3.
        // The skip walk must count the front subtree to find each back.
        let tri =
            |z: f32, flags: u16| tri_bytes(v(0.0, 0.0, z), v(1.0, 0.0, z), v(0.0, 1.0, z), flags);
        let blob = legacy_blob(
            &[
                legacy_node(0x02 | 0x04, v(1.0, 0.0, 0.0), 0.0, &[0, 1]),
                legacy_node(0x02 | 0x04, v(0.0, 1.0, 0.0), 0.0, &[]),
                legacy_node(0, v(0.0, 0.0, 1.0), 0.0, &[2]),
                legacy_node(0, v(0.0, 0.0, 1.0), 1.0, &[0]),
                legacy_node(0, v(0.0, 0.0, 1.0), 2.0, &[1]),
            ],
            &flat(&[tri(0.0, 1), tri(1.0, 2), tri(2.0, 4)]),
            &[],
        );
        let tree = BspTree::load(&blob).expect("loads");

        let root = tree.root();
        assert_eq!(root.front, Some(1));
        assert_eq!(root.back, Some(4));
        let n1 = &tree.nodes()[1];
        assert_eq!(n1.front, Some(2));
        assert_eq!(n1.back, Some(3));

        // triRefs: t0=[0,3], t1=[0,4], t2=[2]; sorted: [0,3] < [0,4] < [2].
        assert_eq!(tree.triangles()[0].flags(), 1);
        assert_eq!(tree.triangles()[1].flags(), 2);
        assert_eq!(tree.triangles()[2].flags(), 4);
        // node0 runs t0,t1; node2 runs t2; nodes 3 and 4 share t0 and t1.
        assert_eq!(root.num_triangles, 2);
        assert_eq!(tree.nodes()[2].num_triangles, 1);
        assert_eq!(tree.nodes()[2].triangles, Some(2));
        let n3 = &tree.nodes()[3];
        let n4 = &tree.nodes()[4];
        assert_eq!(n3.shared, Some(0));
        assert_eq!(n4.shared, Some(1));
        let s3: Vec<_> = tree.node_shared_triangles(n3).map(|t| t.flags()).collect();
        let s4: Vec<_> = tree.node_shared_triangles(n4).map(|t| t.flags()).collect();
        assert_eq!(s3, vec![1]);
        assert_eq!(s4, vec![2]);
    }

    #[test]
    fn legacy_empty_tree_reports_empty() {
        let blob = legacy_blob(&[legacy_node(0, v(0.0, 0.0, 1.0), 0.0, &[])], &[], &[]);
        let tree = BspTree::load(&blob).expect("loads");
        assert!(tree.empty());
        assert!(!tree.can_collide());
        assert_eq!(tree.bounding_box(), &BoundBox::default());
    }

    #[test]
    fn legacy_bad_triangle_index_and_unrefereed_triangles_assert() {
        let t = tri_bytes(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 0);
        // Index 1 with a single triangle.
        let blob = legacy_blob(
            &[legacy_node(0, v(0.0, 0.0, 1.0), 0.0, &[1])],
            &flat(std::slice::from_ref(&t)),
            &[],
        );
        let r = catch_unwind(AssertUnwindSafe(|| BspTree::load(&blob)));
        assert!(r.is_err(), "reference MF_ASSERTs the index is in range");

        // Two triangles but only one is referenced.
        let blob = legacy_blob(
            &[legacy_node(0, v(0.0, 0.0, 1.0), 0.0, &[0])],
            &flat(&[t.clone(), t]),
            &[],
        );
        let r = catch_unwind(AssertUnwindSafe(|| BspTree::load(&blob)));
        assert!(
            r.is_err(),
            "reference MF_ASSERTs every triangle has a referee"
        );
    }

    #[test]
    fn the_comparator_treats_prefixes_as_equal() {
        use std::cmp::Ordering;
        assert_eq!(compare_node_lists(&[0], &[0, 1]), Ordering::Equal);
        assert_eq!(compare_node_lists(&[0, 1], &[0]), Ordering::Equal);
        assert_eq!(compare_node_lists(&[0, 3], &[0, 4]), Ordering::Less);
        assert_eq!(compare_node_lists(&[2], &[0, 4]), Ordering::Greater);
        assert_eq!(compare_node_lists(&[5, 1], &[5, 1]), Ordering::Equal);
    }

    #[test]
    fn a_foreign_version_token_reports_bad_magic_like_the_reference() {
        // getBSPVersion can only answer 0, 2 or INVALID: the masked
        // generic-version branch is dead, so loadBSP's "Unsupported bsp
        // version" default arm is unreachable through a token (ported as
        // written; the error's message is probed in bad_magic...).
        let blob = words(&[BSP_FILE_MAGIC | (3 << 24)]);
        assert_eq!(
            bsp_version(&blob),
            BSP_FILE_INVALID_VERSION,
            "the dead masked branch never answers"
        );
        assert_eq!(BspTree::load(&blob), Err(BspLoadError::BadMagic));
    }

    #[test]
    fn a_legacy_back_only_node_links_to_the_next_node() {
        // flags byte 0x04 (HAS_BACK without HAS_FRONT): the back child is
        // simply the following node (`bsp.cpp:1560-1591`'s else arm).
        let t0 = tri_bytes(v(0.0, 0.0, 0.0), v(1.0, 0.0, 0.0), v(0.0, 1.0, 0.0), 1);
        let t1 = tri_bytes(v(2.0, 0.0, 0.0), v(3.0, 0.0, 0.0), v(2.0, 1.0, 0.0), 2);
        let blob = legacy_blob(
            &[
                legacy_node(0x04, v(1.0, 0.0, 0.0), 0.0, &[0]),
                legacy_node(0, v(0.0, 1.0, 0.0), 4.0, &[1]),
            ],
            &flat(&[t0, t1]),
            &[],
        );
        let tree = BspTree::load(&blob).expect("loads");
        assert_eq!(tree.root().back, Some(1));
        assert_eq!(tree.root().front, None);
        // Both triangles referee: t0 by [0], t1 by [1]; scan order keeps
        // t0 first with each node owning its own run.
        assert_eq!(tree.num_triangles(), 2);
        assert_eq!(tree.nodes()[0].triangles, Some(0));
        assert_eq!(tree.nodes()[1].triangles, Some(1));
        assert_eq!(tree.node_triangles(&tree.nodes()[0]).len(), 1);
    }

    /// A three-node ray fixture: the root splits at the z=1 plane, the
    /// front leaf owns tA at z=2, the back leaf owns tB at z=0.
    fn ray_blob(tri_flags: [u16; 2]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(&words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 2, 3, 0]));
        b.extend(&v3_bytes(v(0.0, 0.0, 0.0)));
        b.extend(&v3_bytes(v(1.0, 1.0, 2.0)));
        b.extend(&[0u8; 8]);
        b.extend(&tri_bytes(
            v(0.0, 0.0, 2.0),
            v(1.0, 0.0, 2.0),
            v(0.0, 1.0, 2.0),
            tri_flags[0],
        ));
        b.extend(&tri_bytes(
            v(0.0, 0.0, 0.0),
            v(1.0, 0.0, 0.0),
            v(0.0, 1.0, 0.0),
            tri_flags[1],
        ));
        b.extend(&node_bytes(
            1,
            2,
            v(0.0, 0.0, 1.0),
            1.0,
            OFFSET_32BIT_NULL,
            0,
            OFFSET_32BIT_NULL,
            0,
        ));
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 0.0, 1.0),
            2.0,
            0,
            1,
            OFFSET_32BIT_NULL,
            0,
        ));
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 0.0, 1.0),
            0.0,
            1,
            1,
            OFFSET_32BIT_NULL,
            0,
        ));
        b
    }

    struct CountingVisitor {
        allowed: u32,
        seen: Vec<(u16, u32)>,
    }

    impl CountingVisitor {
        fn accepting() -> CountingVisitor {
            CountingVisitor {
                allowed: u32::MAX,
                seen: Vec::new(),
            }
        }

        fn rejecting() -> CountingVisitor {
            CountingVisitor {
                allowed: 0,
                seen: Vec::new(),
            }
        }
    }

    impl CollisionVisitor for CountingVisitor {
        fn visit(&mut self, hit_triangle: &WorldTriangle, dist: f32) -> bool {
            self.seen.push((hit_triangle.flags(), dist.to_bits()));
            self.allowed > 0
        }
    }

    #[test]
    fn the_ray_walk_answers_the_nearest_hit_first() {
        let tree = BspTree::load(&ray_blob([0, 0])).expect("loads");

        // Down from above: the front side is scanned first and confirms
        // tA at a quarter of the interval.
        let mut dist = 1.0;
        let hit = tree
            .intersects(v(0.5, 0.5, 3.0), v(0.5, 0.5, -1.0), &mut dist, None)
            .expect("hits");
        assert_eq!(hit.flags(), 0);
        assert_eq!(hit.v0(), v(0.0, 0.0, 2.0), "tA is the hit");
        assert_eq!(dist, 0.25);
        // The hit point is start + dist * (end - start).
        let point = v(0.5, 0.5, 3.0) + (v(0.5, 0.5, -1.0) - v(0.5, 0.5, 3.0)) * dist;
        assert_eq!(point, v(0.5, 0.5, 2.0));

        // Up from below: the back side confirms tB first.
        let mut dist = 1.0;
        let hit = tree
            .intersects(v(0.5, 0.5, -3.0), v(0.5, 0.5, 3.0), &mut dist, None)
            .expect("hits");
        assert_eq!(hit.v0(), v(0.0, 0.0, 0.0), "tB is the hit");
        assert_eq!(dist, 0.5);
    }

    #[test]
    fn the_input_dist_is_the_search_limit() {
        let tree = BspTree::load(&ray_blob([0, 0])).expect("loads");

        // A limit short of the tA hit (0.25) misses both planes.
        let mut dist = 0.1;
        assert_eq!(
            tree.intersects(v(0.5, 0.5, 3.0), v(0.5, 0.5, -1.0), &mut dist, None),
            None,
            "both hits lie beyond the limit"
        );
        assert_eq!(dist, 0.1, "the caller's value survives a miss");

        // A limit past it confirms tA and narrows dist.
        let mut dist = 0.8;
        assert!(
            tree.intersects(v(0.5, 0.5, 3.0), v(0.5, 0.5, -1.0), &mut dist, None)
                .is_some()
        );
        assert_eq!(dist, 0.25);
    }

    #[test]
    fn the_ray_misses_through_a_side_it_does_not_touch() {
        // x=5 is outside both leaves' triangles: the walk descends the
        // start side, revisits the root for the end side and finds
        // nothing anywhere.
        let tree = BspTree::load(&ray_blob([0, 0])).expect("loads");
        let mut dist = 1.0;
        assert_eq!(
            tree.intersects(v(5.0, 0.5, 3.0), v(5.0, 0.5, -1.0), &mut dist, None),
            None
        );
    }

    #[test]
    fn a_rejecting_visitor_restores_and_keeps_scanning() {
        let mut b = Vec::new();
        b.extend(&words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 2, 1, 0]));
        b.extend(&v3_bytes(v(0.0, 0.0, 0.0)));
        b.extend(&v3_bytes(v(1.0, 1.0, 2.0)));
        b.extend(&[0u8; 8]);
        b.extend(&tri_bytes(
            v(0.0, 0.0, 2.0),
            v(1.0, 0.0, 2.0),
            v(0.0, 1.0, 2.0),
            0x0001,
        ));
        b.extend(&tri_bytes(
            v(0.0, 0.0, 1.0),
            v(1.0, 0.0, 1.0),
            v(0.0, 1.0, 1.0),
            0x0002,
        ));
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 0.0, 1.0),
            1.0,
            0,
            2,
            OFFSET_32BIT_NULL,
            0,
        ));
        let tree = BspTree::load(&b).expect("loads");

        // The null visitor takes the first (closest) candidate: tA.
        let mut dist = 1.0;
        let hit = tree
            .intersects(v(0.5, 0.5, 3.0), v(0.5, 0.5, -1.0), &mut dist, None)
            .expect("hits");
        assert_eq!(hit.v0(), v(0.0, 0.0, 2.0));

        // A rejecting visitor is consulted per candidate (tA then tB)
        // and every rejection restores the previous dist: no hit.
        let mut visitor = CountingVisitor::rejecting();
        let mut dist = 1.0;
        assert_eq!(
            tree.intersects(
                v(0.5, 0.5, 3.0),
                v(0.5, 0.5, -1.0),
                &mut dist,
                Some(&mut visitor)
            ),
            None
        );
        assert_eq!(dist, 1.0);
        assert_eq!(
            visitor.seen,
            vec![(0x0001, 0.25f32.to_bits()), (0x0002, 0.5f32.to_bits())],
            "both candidates were offered, with their hit fractions"
        );

        // An accepting visitor takes tA (the ray descends from z=3, so
        // the z=2 plane is nearer at fraction 0.25); tB's farther hit can
        // never beat the running dist, so it is not even offered.
        let mut visitor = CountingVisitor::accepting();
        let mut dist = 1.0;
        let hit = tree
            .intersects(
                v(0.5, 0.5, 3.0),
                v(0.5, 0.5, -1.0),
                &mut dist,
                Some(&mut visitor),
            )
            .expect("hits");
        assert_eq!(hit.v0(), v(0.0, 0.0, 2.0));
        assert_eq!(dist, 0.25);
        assert_eq!(visitor.seen, vec![(0x0001, 0.25f32.to_bits())]);
    }

    #[test]
    fn not_in_bsp_triangles_are_skipped_by_the_ray_walk() {
        let tree =
            BspTree::load(&ray_blob([TRIANGLE_NOT_IN_BSP, TRIANGLE_NOT_IN_BSP])).expect("loads");
        let mut dist = 1.0;
        assert_eq!(
            tree.intersects(v(0.5, 0.5, 3.0), v(0.5, 0.5, -1.0), &mut dist, None),
            None,
            "both planes are NOT_IN_BSP"
        );
        // Only the far one is skipped: the near one still answers.
        let tree = BspTree::load(&ray_blob([TRIANGLE_NOT_IN_BSP, 0])).expect("loads");
        let mut dist = 1.0;
        let hit = tree
            .intersects(v(0.5, 0.5, -3.0), v(0.5, 0.5, 3.0), &mut dist, None)
            .expect("hits");
        assert_eq!(hit.v0(), v(0.0, 0.0, 0.0));
    }

    #[test]
    fn a_node_without_a_triangle_run_yields_no_triangles() {
        // The empty current-format tree's single node has no run; both
        // run accessors answer empty.
        let mut b = Vec::new();
        b.extend(&words(&[BSP_FILE_CUR_TOKEN, 16, 40, 40, 0, 1, 0]));
        b.extend(&v3_bytes(v(0.0, 0.0, 0.0)));
        b.extend(&v3_bytes(v(0.0, 0.0, 0.0)));
        b.extend(&[0u8; 8]);
        b.extend(&node_bytes(
            OFFSET_32BIT_NULL,
            OFFSET_32BIT_NULL,
            v(0.0, 0.0, 1.0),
            0.0,
            OFFSET_32BIT_NULL,
            0,
            OFFSET_32BIT_NULL,
            0,
        ));
        let tree = BspTree::load(&b).expect("loads");
        assert!(tree.node_triangles(tree.root()).is_empty());
        assert_eq!(tree.node_shared_triangles(tree.root()).count(), 0);
    }

    #[test]
    fn the_prism_walk_hits_the_translated_face() {
        let tree = BspTree::load(&ray_blob([0, 0])).expect("loads");

        // A z=3 triangle swept down by 4: the prism crosses the root's
        // z=1 plane. The root has no own run; the walk scans the back
        // leaf first (own run, then back, then front) and its tB — whose
        // slice the prism covers — confirms the hit.
        let tri = WorldTriangle::new(v(0.0, 0.0, 3.0), v(1.0, 0.0, 3.0), v(0.0, 1.0, 3.0), 0x0005);
        assert!(tree.intersects_sweep(&tri, v(0.0, 0.0, -4.0), None));

        // The same triangle swept away above the tree: the back and front
        // leaves both refuse.
        assert!(!tree.intersects_sweep(&tri, v(0.0, 0.0, 10.0), None));

        // A visitor sees the accepted TREE candidate (tB, flags 0 — the
        // input triangle's own flags are irrelevant here) with the
        // reference's constant 0.0 dist.
        let mut visitor = CountingVisitor::accepting();
        assert!(tree.intersects_sweep(&tri, v(0.0, 0.0, -4.0), Some(&mut visitor)));
        assert_eq!(visitor.seen, vec![(0, 0.0f32.to_bits())]);

        // A rejecting visitor keeps scanning: the back leaf's tB and then
        // the front leaf's tA are offered before the walk answers MISS.
        let mut rejecting = CountingVisitor::rejecting();
        assert!(!tree.intersects_sweep(&tri, v(0.0, 0.0, -4.0), Some(&mut rejecting)));
        assert_eq!(
            rejecting.seen,
            vec![(0, 0.0f32.to_bits()), (0, 0.0f32.to_bits())]
        );
    }

    #[test]
    fn the_prism_walk_walks_windows_on_the_way_down() {
        let tree = BspTree::load(&ray_blob([0, 0])).expect("loads");

        // Prism z 0.5..1.5 straddles the root's plane but reaches neither
        // tA (z=2) nor tB (z=0): the back leaf (min < TOL) and the front
        // leaf (max > -TOL) are both entered and both miss.
        let tri = WorldTriangle::new(v(0.2, 0.2, 1.5), v(0.7, 0.2, 1.5), v(0.2, 0.7, 1.5), 0);
        assert!(!tree.intersects_sweep(&tri, v(0.0, 0.0, -1.0), None));

        // Swept down to z=-0.7 the prism covers tB's plane: the back leaf
        // (entered before the front one) confirms the hit.
        assert!(tree.intersects_sweep(&tri, v(0.0, 0.0, -2.2), None));
    }

    #[test]
    fn the_static_overlap_walk_descends_the_first_vertex_side_first() {
        let tree = BspTree::load(&ray_blob([0, 0])).expect("loads");

        // A triangle hanging below the root's plane (d0 < 0) with a single
        // vertex above: the back subtree (tB) is descended BEFORE the
        // front side or the root's own run, and it confirms the overlap.
        let low = WorldTriangle::new(v(0.2, 0.2, -1.0), v(0.5, 0.2, -1.0), v(0.2, 0.5, 1.0), 0);
        assert!(tree.intersects_triangle(&low));

        // A triangle entirely above the root's plane only ever reaches
        // the front subtree and the root's own run (both tA) — and misses.
        let above = WorldTriangle::new(v(0.2, 0.2, 3.0), v(0.5, 0.2, 3.0), v(0.2, 0.5, 5.0), 0);
        assert!(!tree.intersects_triangle(&above));

        // A triangle straddling the root's plane but overlapping nothing
        // (x far outside both triangles) is answered MISS.
        let outside = WorldTriangle::new(v(5.0, 5.0, -1.0), v(5.5, 5.0, -1.0), v(5.0, 5.5, 1.0), 0);
        assert!(!tree.intersects_triangle(&outside));
    }

    #[test]
    fn not_in_bsp_triangles_are_skipped_by_the_sweeps() {
        // tA is NOT_IN_BSP: the prism sweep skips it in the root's own run
        // and the front leaf, then confirms tB through the back leaf.
        let tree = BspTree::load(&ray_blob([TRIANGLE_NOT_IN_BSP, 0])).expect("loads");
        let tri = WorldTriangle::new(v(0.0, 0.0, 3.0), v(1.0, 0.0, 3.0), v(0.0, 1.0, 3.0), 0);
        assert!(tree.intersects_sweep(&tri, v(0.0, 0.0, -4.0), None));
        // The static overlap walk skips the skipped triangle the same way:
        // a triangle overlapping tA's spot above the plane finds nothing.
        let high = WorldTriangle::new(v(0.2, 0.2, 3.0), v(0.5, 0.2, 3.0), v(0.2, 0.5, 5.0), 0);
        assert!(!tree.intersects_triangle(&high));

        // tB is NOT_IN_BSP: a prism entirely behind the root's plane has
        // its only candidate skipped and the sweep misses.
        let tree = BspTree::load(&ray_blob([0, TRIANGLE_NOT_IN_BSP])).expect("loads");
        let low = WorldTriangle::new(v(0.2, 0.2, -0.5), v(0.7, 0.2, -0.5), v(0.2, 0.7, -0.5), 0);
        assert!(!tree.intersects_sweep(&low, v(0.0, 0.0, -0.5), None));
    }

    #[test]
    fn min3_and_max3_follow_the_reference_ternary_chains() {
        assert_eq!(min3(1.0, 2.0, 3.0), 1.0);
        assert_eq!(min3(2.0, 1.0, 3.0), 1.0);
        assert_eq!(min3(3.0, 2.0, 1.0), 1.0);
        assert_eq!(min3(2.0, 2.0, 2.0), 2.0);
        assert_eq!(max3(1.0, 2.0, 3.0), 3.0);
        assert_eq!(max3(3.0, 2.0, 1.0), 3.0);
        assert_eq!(max3(2.0, 3.0, 1.0), 3.0);
        assert_eq!(max3(2.0, 2.0, 2.0), 2.0);
    }
}
