//! BW-10-02-03 the dynamic scene provider: the server-slice port of
//! `lib/space/dynamic_scene_provider.{hpp,cpp}` — registering movable and
//! removable objects into a pair of dynamic loose octrees (collision and
//! visibility) and answering spatial queries over the visibility tree.
//!
//! Since BW-10-02-04 the stored objects are the real `scene::SceneObject`
//! typed handles (the reference `objects_` collection, read back through
//! `objectLookup_`); the object-change notifications and the
//! collision/intersect view wiring remain unported (BW-10-02-05).

use foundation::math::{BoundBox, Matrix, Vector3};
use scene::SceneObject;

use crate::loose_octree::{DynamicLooseOctree, INVALID_NODE, NodeIndex};

/// `Handle<24, 8>` (`dynamic_scene_provider_types.hpp:10`): 24 index bits,
/// 8 generation bits inside a `uint32`.
pub const HANDLE_INDEX_BITS: u32 = 24;
pub const HANDLE_GENERATION_BITS: u32 = 8;
/// `Handle::INVALID_INDEX` = `(1 << indexBits) - 1`.
pub const HANDLE_INVALID_INDEX: u32 = (1 << HANDLE_INDEX_BITS) - 1;
/// `HandleTable::DEFAULT_GROW_BY` (`object_pool.hpp:119` area).
pub const DEFAULT_HANDLE_GROW_BY: usize = 50;

/// `BW::Handle<24, 8>`: an index/generation pair (`object_pool.hpp:36-102`).
/// Equality matches the reference `operator==`: invalid handles are all
/// equal regardless of generation, valid ones must match both fields.
#[derive(Debug, Clone, Copy)]
pub struct DynamicObjectHandle {
    index: u32,
    generation: u32,
}

impl DynamicObjectHandle {
    /// The default handle: invalid index, zero generation.
    pub const fn new_invalid() -> Self {
        Self {
            index: HANDLE_INVALID_INDEX,
            generation: 0,
        }
    }

    pub const fn from_parts(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// `operator baseType()`: `index_ | (generation_ << GENERATION_SHIFT)`.
    pub fn as_raw(self) -> u32 {
        self.index | (self.generation << HANDLE_INDEX_BITS)
    }

    pub fn index(self) -> u32 {
        self.index
    }

    pub fn generation(self) -> u32 {
        self.generation
    }

    /// `handle.isValid()` is `index_ != INVALID_INDEX` — generation is not
    /// consulted here, only the table does that.
    pub fn is_index_valid(self) -> bool {
        self.index != HANDLE_INVALID_INDEX
    }
}

impl PartialEq for DynamicObjectHandle {
    fn eq(&self, other: &Self) -> bool {
        if self.index != other.index {
            return false;
        }
        if self.index == HANDLE_INVALID_INDEX {
            return true;
        }
        self.generation == other.generation
    }
}

impl Eq for DynamicObjectHandle {}

/// `HandleTable<DynamicObjectHandle>::PoolEntry`.
#[derive(Debug, Clone, Copy, Default)]
struct PoolEntry {
    generation: u32,
    next_free: u32,
}

/// `HandleTable<DynamicObjectHandle>` (`object_pool.hpp:119-276`): a FIFO
/// free list over generation-tagged entries. `create` hands out the oldest
/// freed slot with its current generation; `release` bumps the generation
/// (wrapping at `1 << GENERATION_BITS`) and recycles the slot.
#[derive(Debug, Clone)]
pub struct HandleTable {
    entries: Vec<PoolEntry>,
    first_free: u32,
    last_free: u32,
    grow_by: usize,
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}

impl HandleTable {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            first_free: HANDLE_INVALID_INDEX,
            last_free: HANDLE_INVALID_INDEX,
            grow_by: DEFAULT_HANDLE_GROW_BY,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `HandleTable::grow` (`object_pool.hpp:157-180`): append slots and
    /// chain them onto the back of the free list. The size must stay under
    /// `INVALID_INDEX` (an `MF_ASSERT` in the reference).
    fn grow(&mut self, num_objects: usize) {
        let old_size = self.entries.len();
        let new_size = old_size + num_objects;
        assert!(
            new_size < HANDLE_INVALID_INDEX as usize,
            "handle count exceeds the index capacity"
        );

        self.entries.resize(new_size, PoolEntry::default());
        for index in old_size..new_size {
            self.free_list_push(index as u32);
        }
    }

    /// `HandleTable::create` (`object_pool.hpp:182-196`).
    pub fn create(&mut self) -> DynamicObjectHandle {
        if self.first_free == HANDLE_INVALID_INDEX {
            self.grow(self.grow_by);
        }
        assert!(self.first_free != HANDLE_INVALID_INDEX);

        let entry_index = self.free_list_pop();
        let generation = self.entries[entry_index as usize].generation;
        DynamicObjectHandle::from_parts(entry_index, generation)
    }

    /// `HandleTable::release` (`object_pool.hpp:198-217`): bump the
    /// generation and push the slot onto the back of the free list.
    pub fn release(&mut self, handle: DynamicObjectHandle) {
        assert!(self.is_valid(handle), "reference asserts on release");

        let generation_mask = (1 << HANDLE_GENERATION_BITS) - 1;
        let entry = &mut self.entries[handle.index as usize];
        entry.generation = (entry.generation + 1) & generation_mask;
        self.free_list_push(handle.index);
    }

    /// `HandleTable::isValid` (`object_pool.hpp:219-231`).
    pub fn is_valid(&self, handle: DynamicObjectHandle) -> bool {
        if handle.index as usize >= self.entries.len() {
            return false;
        }
        if handle.generation != self.entries[handle.index as usize].generation {
            return false;
        }
        true
    }

    /// `freeListPush` (`object_pool.hpp:238-256`): FIFO queue, add to back.
    fn free_list_push(&mut self, index: u32) {
        if self.last_free != HANDLE_INVALID_INDEX {
            let last = self.last_free as usize;
            self.entries[last].next_free = index;
        }
        self.last_free = index;
        let back = self.last_free as usize;
        self.entries[back].next_free = HANDLE_INVALID_INDEX;

        if self.first_free == HANDLE_INVALID_INDEX {
            self.first_free = self.last_free;
        }
    }

    /// `freeListPop` (`object_pool.hpp:258-276`): FIFO queue, pop the front.
    fn free_list_pop(&mut self) -> u32 {
        assert!(self.first_free != HANDLE_INVALID_INDEX);

        let result = self.first_free;
        self.first_free = self.entries[result as usize].next_free;
        if self.first_free == HANDLE_INVALID_INDEX {
            self.last_free = HANDLE_INVALID_INDEX;
        }
        result
    }
}

/// The per-object tree placement the provider remembers
/// (`DynamicObjectStorageInfo`, `dynamic_scene_provider.hpp:88-92`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DynamicObjectStorageInfo {
    pub collision_node: NodeIndex,
    pub visibility_node: NodeIndex,
}

/// `DEFAULT_METERS_PER_NODE` and `MAX_OCTREE_DEPTH` from
/// `DynamicSceneProvider::resizeScene` (`dynamic_scene_provider.cpp:508-509`).
pub const DEFAULT_METERS_PER_NODE: f32 = 20.0;
pub const MAX_OCTREE_DEPTH: u32 = 8;
/// The constructor's bootstrap trees (`dynamic_scene_provider.cpp:118-121`).
pub const INITIAL_WORLD_SIZE: f32 = 2000.0;
pub const INITIAL_MAX_DEPTH: u32 = 1;

/// `DynamicSceneProvider` (`lib/space/dynamic_scene_provider.hpp`): objects
/// keyed by generational handles, double-buffered update state (writes land
/// in the `latest_*` tables and only become queryable at `on_post_tick`),
/// and separate collision/visibility octrees.
///
/// The reference `removeObject_internal` never calls
/// `dynamicObjectHandles_.release()` — pool slots are never recycled by
/// the provider, so handle indices monotonically grow. That quirk is kept.
#[derive(Debug, Clone)]
pub struct DynamicSceneProvider {
    dynamic_object_handles: HandleTable,
    objects: Vec<SceneObject>,
    object_handles: Vec<DynamicObjectHandle>,
    object_lookup: Vec<usize>,
    object_collision_bbs: Vec<BoundBox>,
    object_visibility_bbs: Vec<BoundBox>,
    object_world_xform: Vec<Matrix>,
    object_storage_info: Vec<DynamicObjectStorageInfo>,
    latest_object_collision_bbs: Vec<BoundBox>,
    latest_object_visibility_bbs: Vec<BoundBox>,
    latest_object_world_xform: Vec<Matrix>,
    object_dirty_flags: Vec<u8>,
    collision_tree: DynamicLooseOctree,
    collision_tree_contents: Vec<Vec<DynamicObjectHandle>>,
    visibility_tree: DynamicLooseOctree,
    visibility_tree_contents: Vec<Vec<DynamicObjectHandle>>,
    num_dirty_objects: u32,
    partition_size_hint: f32,
}

impl Default for DynamicSceneProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// `BW::LookUpTable<T>`: indexing past the end default-fills the slots in
/// between (`cstdmf/lookup_table.hpp`).
fn table_set<T: Default + Clone>(table: &mut Vec<T>, index: usize, value: T) {
    if index >= table.len() {
        table.resize(index + 1, T::default());
    }
    table[index] = value;
}

fn table_get_mut<T: Default + Clone>(table: &mut Vec<T>, index: usize) -> &mut T {
    if index >= table.len() {
        table.resize(index + 1, T::default());
    }
    &mut table[index]
}

impl DynamicSceneProvider {
    /// The reference constructor (`dynamic_scene_provider.cpp:113-126`).
    pub fn new() -> Self {
        let mut collision_tree = DynamicLooseOctree::default();
        collision_tree.initialise(Vector3::ZERO, INITIAL_WORLD_SIZE, INITIAL_MAX_DEPTH);
        let mut visibility_tree = DynamicLooseOctree::default();
        visibility_tree.initialise(Vector3::ZERO, INITIAL_WORLD_SIZE, INITIAL_MAX_DEPTH);

        Self {
            dynamic_object_handles: HandleTable::new(),
            objects: Vec::new(),
            object_handles: Vec::new(),
            object_lookup: Vec::new(),
            object_collision_bbs: Vec::new(),
            object_visibility_bbs: Vec::new(),
            object_world_xform: Vec::new(),
            object_storage_info: Vec::new(),
            latest_object_collision_bbs: Vec::new(),
            latest_object_visibility_bbs: Vec::new(),
            latest_object_world_xform: Vec::new(),
            object_dirty_flags: Vec::new(),
            collision_tree,
            collision_tree_contents: Vec::new(),
            visibility_tree,
            visibility_tree_contents: Vec::new(),
            num_dirty_objects: 0,
            partition_size_hint: -1.0,
        }
    }

    /// `setPartitionSizeHint` (`dynamic_scene_provider.cpp:498-501`).
    pub fn set_partition_size_hint(&mut self, partition_size_hint: f32) {
        self.partition_size_hint = partition_size_hint;
    }

    /// `addObject` (`dynamic_scene_provider.cpp:160-171` minus the
    /// object-change notification, which belongs to the scene views).
    pub fn add_object(
        &mut self,
        object: SceneObject,
        world_xform: &Matrix,
        collision_bb: &BoundBox,
        visibility_bb: &BoundBox,
    ) -> DynamicObjectHandle {
        self.insert_object_internal(object, world_xform, collision_bb, visibility_bb)
    }

    /// `removeObject` (`dynamic_scene_provider.cpp:174-187`): an invalid
    /// handle is a no-op (the reference asserts then returns), removal is
    /// immediate for queries.
    pub fn remove_object(&mut self, handle: DynamicObjectHandle) {
        if !self.dynamic_object_handles.is_valid(handle) {
            return;
        }

        self.remove_object_internal(handle);
    }

    /// `updateObject` (`dynamic_scene_provider.cpp:190-202`): invalid
    /// handle is a no-op; the new state is staged and only committed by
    /// `on_post_tick`.
    pub fn update_object(
        &mut self,
        handle: DynamicObjectHandle,
        world_xform: &Matrix,
        collision_bb: &BoundBox,
        visible_bb: &BoundBox,
    ) {
        if !self.dynamic_object_handles.is_valid(handle) {
            return;
        }

        self.update_object_internal(handle, world_xform, collision_bb, visible_bb);
    }

    /// `onPostTick` (`dynamic_scene_provider.cpp:330-402`): the deferred
    /// rebuild. Without dirty objects nothing happens; otherwise both
    /// trees are reinitialised, dirty objects take their latest state, and
    /// every live object is reinserted before the hierarchy is recomputed.
    pub fn on_post_tick(&mut self) {
        if self.num_dirty_objects == 0 {
            return;
        }

        // Reset entire trees and reinsert everything
        self.collision_tree.reinitialise();
        self.visibility_tree.reinitialise();

        for contents in self.collision_tree_contents.iter_mut() {
            contents.clear();
        }
        for contents in self.visibility_tree_contents.iter_mut() {
            contents.clear();
        }

        // Feed all the latest data into the core trees
        let mut updated_objects: Vec<SceneObject> =
            Vec::with_capacity(self.num_dirty_objects as usize);
        for handle in self.object_handles.iter().copied() {
            let index = handle.index() as usize;
            if self.object_dirty_flags[index] != 0 {
                self.object_dirty_flags[index] = 0;

                self.object_collision_bbs[index] = self.latest_object_collision_bbs[index];
                self.object_visibility_bbs[index] = self.latest_object_visibility_bbs[index];
                self.object_world_xform[index] = self.latest_object_world_xform[index];

                updated_objects.push(self.objects[self.object_lookup[index]].clone());
            }
        }

        // Now insert all dynamic objects into these trees again and update
        // their tree info
        for handle in self.object_handles.iter().copied() {
            let mut storage = self.object_storage_info[handle.index() as usize];
            Self::insert_object_node(
                handle,
                &self.object_collision_bbs,
                &mut self.collision_tree,
                &mut self.collision_tree_contents,
                &mut storage.collision_node,
                false,
            );
            self.object_storage_info[handle.index() as usize] = storage;
        }

        for handle in self.object_handles.iter().copied() {
            let mut storage = self.object_storage_info[handle.index() as usize];
            Self::insert_object_node(
                handle,
                &self.object_visibility_bbs,
                &mut self.visibility_tree,
                &mut self.visibility_tree_contents,
                &mut storage.visibility_node,
                false,
            );
            self.object_storage_info[handle.index() as usize] = storage;
        }

        self.collision_tree.update_heirarchy();
        self.visibility_tree.update_heirarchy();

        self.num_dirty_objects = 0;
    }

    /// The AABB face of `intersect` (`dynamic_scene_provider.cpp:449-495`,
    /// with an `AABB` in place of the `ConvexHull` the reference hull face
    /// uses): visibility-tree candidates filtered by each object's own
    /// visibility bound. Each hit resolves through the reference chain
    /// `objects_[objectLookup_[handle.index_]]` and is appended in tree
    /// walk order — the reference appends into the intersection set with
    /// no dedup (`intersection.insert`, cpp:479-482).
    pub fn query_region(&self, region: &BoundBox) -> Vec<SceneObject> {
        let mut intersection = Vec::new();
        for node_ref in self.visibility_tree.intersect_aabb(region) {
            let contents = &self.visibility_tree_contents[node_ref as usize];
            for handle in contents.iter().copied() {
                let bb = &self.object_visibility_bbs[handle.index() as usize];

                if bb.inside_out() {
                    continue;
                }

                if region.intersects(bb) {
                    let slot = self.object_lookup[handle.index() as usize];
                    intersection.push(self.objects[slot].clone());
                }
            }
        }
        intersection
    }

    /// Resolve a handle back to its registered scene object — the
    /// reference read chain `objects_[objectLookup_[handle.index_]]`
    /// (`dynamic_scene_provider.cpp:183`, `:289`, `:366`).
    pub fn object(&self, handle: DynamicObjectHandle) -> SceneObject {
        let index = handle.index() as usize;
        assert!(
            self.dynamic_object_handles.is_valid(handle),
            "object() on an invalid handle"
        );
        assert!(
            self.object_lookup[index] != usize::MAX,
            "object() on a removed handle"
        );
        self.objects[self.object_lookup[index]].clone()
    }

    /// `resizeScene` (`dynamic_scene_provider.cpp:504-569`): depth from the
    /// scene's widest dimension and the partition hint, then rebuild both
    /// trees from the old contents.
    pub fn resize_scene(&mut self, scene_bb: &BoundBox) {
        let node_size = if self.partition_size_hint > 0.0 {
            self.partition_size_hint
        } else {
            DEFAULT_METERS_PER_NODE
        };

        let width = scene_bb.width();
        let max_dimension = width.x.max(width.y).max(width.z);
        let nodes_per_edge = (max_dimension / node_size) as u32;
        let mut max_depth = log2ceil(nodes_per_edge);
        max_depth = max_depth.clamp(1, MAX_OCTREE_DEPTH);

        let mut old_collision_contents = std::mem::take(&mut self.collision_tree_contents);
        let mut old_visibility_contents = std::mem::take(&mut self.visibility_tree_contents);

        self.collision_tree.reset();
        self.visibility_tree.reset();

        self.collision_tree
            .initialise(scene_bb.centre(), max_dimension, max_depth);
        self.visibility_tree
            .initialise(scene_bb.centre(), max_dimension, max_depth);

        // Now insert all dynamic objects into these trees again and update
        // their tree info
        for contents in old_collision_contents.drain(..) {
            for handle in contents.into_iter() {
                let mut storage = self.object_storage_info[handle.index() as usize];
                Self::insert_object_node(
                    handle,
                    &self.object_collision_bbs,
                    &mut self.collision_tree,
                    &mut self.collision_tree_contents,
                    &mut storage.collision_node,
                    false,
                );
                self.object_storage_info[handle.index() as usize] = storage;
            }
        }
        self.collision_tree.update_heirarchy();

        for contents in old_visibility_contents.drain(..) {
            for handle in contents.into_iter() {
                let mut storage = self.object_storage_info[handle.index() as usize];
                Self::insert_object_node(
                    handle,
                    &self.object_visibility_bbs,
                    &mut self.visibility_tree,
                    &mut self.visibility_tree_contents,
                    &mut storage.visibility_node,
                    false,
                );
                self.object_storage_info[handle.index() as usize] = storage;
            }
        }
        self.visibility_tree.update_heirarchy();
    }

    /// The number of live objects (for tests and callers).
    pub fn num_objects(&self) -> usize {
        self.objects.len()
    }

    /// Whether a handle is currently table-valid. Note the reference
    /// quirk: removal does not release the pool entry, so removed handles
    /// stay table-valid until the generation moves on.
    pub fn handle_is_valid(&self, handle: DynamicObjectHandle) -> bool {
        self.dynamic_object_handles.is_valid(handle)
    }

    /// `Detail::insertObjectNode` (`dynamic_scene_provider.cpp:218-235`):
    /// insert the bound, file the handle under the leaf's data reference,
    /// optionally repair the bounds up the tree.
    fn insert_object_node(
        handle: DynamicObjectHandle,
        bb_collection: &[BoundBox],
        tree: &mut DynamicLooseOctree,
        contents: &mut Vec<Vec<DynamicObjectHandle>>,
        new_index: &mut NodeIndex,
        update_heirachy: bool,
    ) {
        let node_index = tree.insert_bound_box(&bb_collection[handle.index() as usize]);
        if node_index != INVALID_NODE {
            let data_ref = tree.data_on_leaf(node_index);
            table_get_mut(contents, data_ref as usize).push(handle);
            if update_heirachy {
                tree.update_node_bound(node_index);
            }
        }

        *new_index = node_index;
    }

    /// `Detail::removeNodeContents` (`dynamic_scene_provider.cpp:237-247`):
    /// swap-remove the handle (an `MF_ASSERT` fires if it is missing).
    fn remove_node_contents(contents: &mut Vec<DynamicObjectHandle>, handle: DynamicObjectHandle) {
        let find_result = contents
            .iter()
            .position(|&h| h == handle)
            .expect("reference asserts the handle is in the node");
        let last = contents.len() - 1;
        contents.swap(find_result, last);
        contents.pop();
    }

    /// `Detail::removeObjectNode` (`dynamic_scene_provider.cpp:249-259`).
    fn remove_object_node(
        handle: DynamicObjectHandle,
        old_index: NodeIndex,
        tree: &DynamicLooseOctree,
        contents: &mut [Vec<DynamicObjectHandle>],
    ) {
        if old_index == INVALID_NODE {
            return;
        }

        let data_ref = tree.data_on_leaf(old_index);
        Self::remove_node_contents(&mut contents[data_ref as usize], handle);
    }

    /// `insertObject_internal` (`dynamic_scene_provider.cpp:572-600`).
    fn insert_object_internal(
        &mut self,
        object: SceneObject,
        world_xform: &Matrix,
        collision_bb: &BoundBox,
        visibility_bb: &BoundBox,
    ) -> DynamicObjectHandle {
        let handle = self.dynamic_object_handles.create();

        self.objects.push(object);
        self.object_handles.push(handle);

        let index = handle.index() as usize;
        table_set(&mut self.object_lookup, index, self.objects.len() - 1);
        table_set(&mut self.object_collision_bbs, index, *collision_bb);
        table_set(&mut self.object_visibility_bbs, index, *visibility_bb);
        table_set(&mut self.object_world_xform, index, *world_xform);
        table_set(&mut self.object_dirty_flags, index, 0);

        // Insert the object into the trees
        let mut tree_info = DynamicObjectStorageInfo::default();
        Self::insert_object_node(
            handle,
            &self.object_collision_bbs,
            &mut self.collision_tree,
            &mut self.collision_tree_contents,
            &mut tree_info.collision_node,
            true,
        );
        Self::insert_object_node(
            handle,
            &self.object_visibility_bbs,
            &mut self.visibility_tree,
            &mut self.visibility_tree_contents,
            &mut tree_info.visibility_node,
            true,
        );

        table_set(&mut self.object_storage_info, index, tree_info);

        handle
    }

    /// `updateObject_internal` (`dynamic_scene_provider.cpp:603-623`):
    /// stage into the latest tables; count a newly-dirty object once.
    fn update_object_internal(
        &mut self,
        handle: DynamicObjectHandle,
        world_xform: &Matrix,
        collision_bb: &BoundBox,
        visible_bb: &BoundBox,
    ) {
        let index = handle.index() as usize;

        table_set(&mut self.latest_object_world_xform, index, *world_xform);
        table_set(&mut self.latest_object_collision_bbs, index, *collision_bb);
        table_set(&mut self.latest_object_visibility_bbs, index, *visible_bb);

        let dirty = table_get_mut(&mut self.object_dirty_flags, index);
        if *dirty == 0 {
            // Trigger a rebuild at the end of tick
            // and allow smart sizing of update buffers
            self.num_dirty_objects += 1;
        }
        *dirty = 1;
    }

    /// `removeObject_internal` (`dynamic_scene_provider.cpp:625-652`). The
    /// tree contents entries are left in place for the next occupant, the
    /// object slot is swap-removed with its lookup fixed up, and — as in
    /// the reference — the handle pool entry is NOT released.
    fn remove_object_internal(&mut self, handle: DynamicObjectHandle) {
        let index = handle.index() as usize;

        // Remove the objects from the trees
        let storage = self.object_storage_info[index];
        Self::remove_object_node(
            handle,
            storage.collision_node,
            &self.collision_tree,
            &mut self.collision_tree_contents,
        );
        Self::remove_object_node(
            handle,
            storage.visibility_node,
            &self.visibility_tree,
            &mut self.visibility_tree_contents,
        );

        // Remove the object from the list of Objects though.
        // Swap the position of the old object with the outgoing one.
        let removed_object_index = self.object_lookup[index];
        let last = self.objects.len() - 1;
        self.objects[removed_object_index] = self.objects[last].clone();
        let moved_object_handle = self.object_handles[last];
        self.object_handles[removed_object_index] = moved_object_handle;

        table_set(
            &mut self.object_lookup,
            moved_object_handle.index() as usize,
            removed_object_index,
        );
        table_set(&mut self.object_lookup, index, usize::MAX);

        self.objects.pop();
        self.object_handles.pop();
    }
}

/// `BW::log2ceil` (`math/math_extra.cpp:104-119`): the ceiling log2, with
/// zero and one both mapping to zero.
fn log2ceil(x: u32) -> u32 {
    if x <= 1 {
        0
    } else {
        32 - (x - 1).leading_zeros()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scene::SceneObjectFlags;

    fn xform(x: f32, y: f32, z: f32) -> Matrix {
        let mut m = Matrix::identity();
        m.set_translate(x, y, z);
        m
    }

    fn bb(min: [f32; 3], max: [f32; 3]) -> BoundBox {
        BoundBox::new(
            Vector3::new(min[0], min[1], min[2]),
            Vector3::new(max[0], max[1], max[2]),
        )
    }

    /// The registered entity type for the test scene objects.
    struct Entity;

    fn entity(handle: u64) -> SceneObject {
        SceneObject::typed::<Entity>(handle, SceneObjectFlags::new())
    }

    /// Resolve a query result to the registered entity handles, sorted for
    /// comparison.
    fn entity_ids(provider: &DynamicSceneProvider, region: &BoundBox) -> Vec<u64> {
        let mut ids: Vec<u64> = provider
            .query_region(region)
            .iter()
            .map(|object| object.handle())
            .collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn handle_table_hands_out_freed_slots_in_fifo_order() {
        let mut table = HandleTable::new();
        let mut handles = Vec::new();
        for _ in 0..DEFAULT_HANDLE_GROW_BY {
            handles.push(table.create());
        }
        assert_eq!(handles[0].index(), 0);
        assert_eq!(
            handles[DEFAULT_HANDLE_GROW_BY - 1].index(),
            DEFAULT_HANDLE_GROW_BY as u32 - 1
        );

        table.release(handles[0]);
        table.release(handles[1]);
        // FIFO: slot 0 is recycled first, one generation later.
        let c = table.create();
        assert_eq!((c.index(), c.generation()), (0, 1));
        let d = table.create();
        assert_eq!((d.index(), d.generation()), (1, 1));
        // The free list is empty again; the next create grows the pool.
        let e = table.create();
        assert_eq!(e.index(), DEFAULT_HANDLE_GROW_BY as u32);
        assert_eq!(table.len(), DEFAULT_HANDLE_GROW_BY * 2);

        assert!(
            !table.is_valid(handles[0]),
            "stale generation fails validation"
        );
        assert!(table.is_valid(c));
        assert!(table.is_valid(d));
    }

    #[test]
    fn handle_table_wraps_the_generation_at_eight_bits() {
        let mut table = HandleTable::new();
        let handles: Vec<_> = (0..DEFAULT_HANDLE_GROW_BY)
            .map(|_| table.create())
            .collect();
        let mut current = handles[0];
        assert_eq!(current.generation(), 0);
        // Cycle the slot a few times: each release bumps the generation,
        // each recycle hands the bumped one out. Releasing the stale
        // generation instead is an assert (as in the reference).
        for expected in 1..=3u32 {
            table.release(current);
            current = table.create();
            assert_eq!((current.index(), current.generation()), (0, expected));
        }
    }

    #[test]
    fn handle_table_grows_by_the_default_step() {
        let mut table = HandleTable::new();
        for i in 0..DEFAULT_HANDLE_GROW_BY as u32 {
            assert_eq!(table.create().index(), i);
        }
        assert_eq!(table.len(), DEFAULT_HANDLE_GROW_BY);
        assert_eq!(table.create().index(), DEFAULT_HANDLE_GROW_BY as u32);
        assert_eq!(table.len(), DEFAULT_HANDLE_GROW_BY * 2);
    }

    #[test]
    fn handle_invalid_index_fails_table_validation() {
        let table = HandleTable::new();
        assert!(!table.is_valid(DynamicObjectHandle::new_invalid()));
        assert!(!table.is_valid(DynamicObjectHandle::from_parts(0, 0)));
    }

    #[test]
    fn handle_equality_ignores_generation_when_invalid() {
        let a = DynamicObjectHandle::new_invalid();
        let b = DynamicObjectHandle::from_parts(HANDLE_INVALID_INDEX, 7);
        assert_eq!(a, b);
        assert_ne!(a, DynamicObjectHandle::from_parts(0, 0));
        assert_ne!(
            DynamicObjectHandle::from_parts(0, 1),
            DynamicObjectHandle::from_parts(0, 2)
        );
        assert_eq!(
            DynamicObjectHandle::from_parts(3, 4).as_raw(),
            3u32 | (4 << HANDLE_INDEX_BITS)
        );
        assert!(!DynamicObjectHandle::new_invalid().is_index_valid());
    }

    #[test]
    fn constructor_bootstraps_the_reference_trees() {
        let provider = DynamicSceneProvider::new();
        for tree in [&provider.collision_tree, &provider.visibility_tree] {
            assert_eq!(tree.config.world_center, Vector3::ZERO);
            assert_eq!(tree.config.world_size, INITIAL_WORLD_SIZE);
            assert_eq!(tree.config.max_depth, INITIAL_MAX_DEPTH);
            assert_eq!(tree.nodes.len(), 1);
            assert!(tree.empty());
        }
        assert_eq!(provider.num_objects(), 0);
    }

    #[test]
    fn added_objects_answer_region_queries() {
        let mut provider = DynamicSceneProvider::new();
        provider.add_object(
            entity(10),
            &xform(5.0, 0.0, 5.0),
            &bb([4.0, -1.0, 4.0], [6.0, 1.0, 6.0]),
            &bb([4.0, -1.0, 4.0], [6.0, 1.0, 6.0]),
        );
        provider.add_object(
            entity(11),
            &xform(-5.0, 0.0, -5.0),
            &bb([-6.0, -1.0, -6.0], [-4.0, 1.0, -4.0]),
            &bb([-6.0, -1.0, -6.0], [-4.0, 1.0, -4.0]),
        );
        provider.add_object(
            entity(12),
            &xform(5.0, 0.0, -5.0),
            &bb([4.0, -1.0, -6.0], [6.0, 1.0, -4.0]),
            &bb([4.0, -1.0, -6.0], [6.0, 1.0, -4.0]),
        );

        let whole = bb([-20.0, -20.0, -20.0], [20.0, 20.0, 20.0]);
        assert_eq!(entity_ids(&provider, &whole), [10, 11, 12]);

        let east = bb([0.0, -10.0, 0.0], [20.0, 10.0, 20.0]);
        assert_eq!(entity_ids(&provider, &east), [10]);

        let miss = bb([100.0, 100.0, 100.0], [101.0, 101.0, 101.0]);
        assert!(provider.query_region(&miss).is_empty());
    }

    #[test]
    fn queries_match_after_movement_and_removal() {
        // Acceptance: 移动和移除后查询结果匹配.
        let mut provider = DynamicSceneProvider::new();
        provider.add_object(
            entity(1),
            &xform(5.0, 0.0, 5.0),
            &bb([4.0, -1.0, 4.0], [6.0, 1.0, 6.0]),
            &bb([4.0, -1.0, 4.0], [6.0, 1.0, 6.0]),
        );
        let h_b = provider.add_object(
            entity(2),
            &xform(-5.0, 0.0, -5.0),
            &bb([-6.0, -1.0, -6.0], [-4.0, 1.0, -4.0]),
            &bb([-6.0, -1.0, -6.0], [-4.0, 1.0, -4.0]),
        );
        let h_c = provider.add_object(
            entity(3),
            &xform(15.0, 0.0, -5.0),
            &bb([14.0, -1.0, -6.0], [16.0, 1.0, -4.0]),
            &bb([14.0, -1.0, -6.0], [16.0, 1.0, -4.0]),
        );
        assert_eq!((h_b.index(), h_c.index()), (1, 2));

        let east = bb([10.0, -10.0, -10.0], [20.0, 10.0, 10.0]);
        let west = bb([-20.0, -10.0, -20.0], [0.0, 10.0, 0.0]);
        assert_eq!(entity_ids(&provider, &east), [3]);
        assert_eq!(entity_ids(&provider, &west), [2]);

        // Move b east; the commit happens at the post-tick rebuild.
        provider.update_object(
            h_b,
            &xform(15.0, 0.0, -5.0),
            &bb([14.0, -1.0, -6.0], [16.0, 1.0, -4.0]),
            &bb([14.0, -1.0, -6.0], [16.0, 1.0, -4.0]),
        );
        provider.on_post_tick();
        assert!(provider.query_region(&west).is_empty());
        assert_eq!(entity_ids(&provider, &east), [2, 3]);

        // Remove c; queries stop seeing it immediately.
        provider.remove_object(h_c);
        assert_eq!(entity_ids(&provider, &east), [2]);
        assert_eq!(entity_ids(&provider, &east), [2], "repeat is stable");

        // a was never touched.
        let south_east = bb([0.0, -10.0, 0.0], [20.0, 10.0, 20.0]);
        assert_eq!(entity_ids(&provider, &south_east), [1]);
    }

    #[test]
    fn updates_stay_invisible_until_the_post_tick_rebuild() {
        let mut provider = DynamicSceneProvider::new();
        let h = provider.add_object(
            entity(7),
            &xform(-5.0, 0.0, -5.0),
            &bb([-6.0, -1.0, -6.0], [-4.0, 1.0, -4.0]),
            &bb([-6.0, -1.0, -6.0], [-4.0, 1.0, -4.0]),
        );

        provider.update_object(
            h,
            &xform(5.0, 0.0, 5.0),
            &bb([4.0, -1.0, 4.0], [6.0, 1.0, 6.0]),
            &bb([4.0, -1.0, 4.0], [6.0, 1.0, 6.0]),
        );
        let east = bb([0.0, -10.0, 0.0], [20.0, 10.0, 20.0]);
        let west = bb([-20.0, -10.0, -20.0], [0.0, 10.0, 0.0]);
        assert!(
            provider.query_region(&east).is_empty(),
            "staged state is not queryable yet"
        );
        assert_eq!(entity_ids(&provider, &west), [7]);

        provider.on_post_tick();
        assert_eq!(entity_ids(&provider, &east), [7]);
        assert!(provider.query_region(&west).is_empty());

        // A clean tick is a no-op.
        provider.on_post_tick();
        assert_eq!(entity_ids(&provider, &east), [7]);
    }

    #[test]
    fn an_update_then_remove_before_post_tick_is_absorbed() {
        let mut provider = DynamicSceneProvider::new();
        let h_a = provider.add_object(
            entity(1),
            &xform(-5.0, 0.0, -5.0),
            &bb([-6.0, -1.0, -6.0], [-4.0, 1.0, -4.0]),
            &bb([-6.0, -1.0, -6.0], [-4.0, 1.0, -4.0]),
        );
        provider.add_object(
            entity(2),
            &xform(5.0, 0.0, 5.0),
            &bb([4.0, -1.0, 4.0], [6.0, 1.0, 6.0]),
            &bb([4.0, -1.0, 4.0], [6.0, 1.0, 6.0]),
        );
        provider.update_object(
            h_a,
            &xform(5.0, 0.0, -5.0),
            &bb([4.0, -1.0, -6.0], [6.0, 1.0, -4.0]),
            &bb([4.0, -1.0, -6.0], [6.0, 1.0, -4.0]),
        );
        provider.remove_object(h_a);
        provider.on_post_tick();

        let whole = bb([-20.0, -20.0, -20.0], [20.0, 20.0, 20.0]);
        assert_eq!(entity_ids(&provider, &whole), [2]);
        assert_eq!(provider.num_objects(), 1);
    }

    #[test]
    fn removed_handles_keep_pool_validity_and_indices_are_not_recycled() {
        let mut provider = DynamicSceneProvider::new();
        let h = provider.add_object(
            entity(5),
            &xform(0.0, 0.0, 0.0),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
        );
        provider.remove_object(h);
        // Reference quirk: removeObject_internal never releases the pool
        // entry, so the handle stays table-valid.
        assert!(provider.handle_is_valid(h));
        assert_eq!(provider.num_objects(), 0);

        let next = provider.add_object(
            entity(6),
            &xform(0.0, 0.0, 0.0),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
        );
        assert_ne!(
            next.index(),
            h.index(),
            "the free list never sees the removed slot"
        );

        let whole = bb([-20.0, -20.0, -20.0], [20.0, 20.0, 20.0]);
        assert_eq!(entity_ids(&provider, &whole), [6]);
    }

    #[test]
    fn invalid_handles_are_ignored() {
        let mut provider = DynamicSceneProvider::new();
        let stale = DynamicObjectHandle::from_parts(99, 0);
        provider.update_object(
            stale,
            &xform(1.0, 1.0, 1.0),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
        );
        provider.remove_object(stale);
        assert_eq!(provider.num_objects(), 0);
        provider.on_post_tick();
    }

    #[test]
    fn inside_out_visibility_bounds_never_answer_queries() {
        let mut provider = DynamicSceneProvider::new();
        provider.add_object(
            entity(9),
            &xform(0.0, 0.0, 0.0),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
            &BoundBox::default(),
        );
        let whole = bb([-20.0, -20.0, -20.0], [20.0, 20.0, 20.0]);
        assert!(provider.query_region(&whole).is_empty());
        assert_eq!(provider.num_objects(), 1);
    }

    #[test]
    fn resize_scene_rebuilds_with_the_reference_depth() {
        let mut provider = DynamicSceneProvider::new();
        provider.add_object(
            entity(1),
            &xform(45.0, 0.0, 45.0),
            &bb([44.0, -1.0, 44.0], [46.0, 1.0, 46.0]),
            &bb([44.0, -1.0, 44.0], [46.0, 1.0, 46.0]),
        );
        provider.add_object(
            entity(2),
            &xform(-45.0, 0.0, -45.0),
            &bb([-46.0, -1.0, -46.0], [-44.0, 1.0, -44.0]),
            &bb([-46.0, -1.0, -46.0], [-44.0, 1.0, -44.0]),
        );

        // 100m across at 20m per node = 5 nodes per edge -> log2ceil(5) = 3.
        provider.resize_scene(&bb([-50.0, -50.0, -50.0], [50.0, 50.0, 50.0]));
        assert_eq!(provider.visibility_tree.config.max_depth, 3);
        assert_eq!(provider.visibility_tree.config.world_size, 100.0);
        assert_eq!(provider.visibility_tree.config.world_center, Vector3::ZERO);

        let whole = bb([-60.0, -60.0, -60.0], [60.0, 60.0, 60.0]);
        assert_eq!(entity_ids(&provider, &whole), [1, 2]);
        let north_east = bb([40.0, -10.0, 40.0], [50.0, 10.0, 50.0]);
        assert_eq!(entity_ids(&provider, &north_east), [1]);
    }

    #[test]
    fn partition_size_hint_scales_the_node_count() {
        let mut provider = DynamicSceneProvider::new();
        provider.set_partition_size_hint(50.0);
        provider.add_object(
            entity(1),
            &xform(0.0, 0.0, 0.0),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
        );
        // 100m across at 50m per node = 2 nodes per edge -> log2ceil(2) = 1.
        provider.resize_scene(&bb([-50.0, -50.0, -50.0], [50.0, 50.0, 50.0]));
        assert_eq!(provider.visibility_tree.config.max_depth, 1);
        let whole = bb([-60.0, -60.0, -60.0], [60.0, 60.0, 60.0]);
        assert_eq!(entity_ids(&provider, &whole), [1]);
    }

    #[test]
    fn log2ceil_matches_the_reference_bit_trick() {
        assert_eq!(log2ceil(0), 0);
        assert_eq!(log2ceil(1), 0);
        assert_eq!(log2ceil(2), 1);
        assert_eq!(log2ceil(3), 2);
        assert_eq!(log2ceil(4), 2);
        assert_eq!(log2ceil(5), 3);
        assert_eq!(log2ceil(8), 3);
        assert_eq!(log2ceil(9), 4);
        assert_eq!(log2ceil(1024), 10);
    }

    /// Acceptance: 实体可定位对应场景对象.
    #[test]
    fn an_entity_locates_its_scene_object_through_the_handle() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let mut provider = DynamicSceneProvider::new();

        // The entity builds its typed presence and registers it.
        let mut flags = SceneObjectFlags::new();
        flags.set_is_dynamic(true);
        let presence = SceneObject::typed::<Entity>(42, flags);
        let handle = provider.add_object(
            presence.clone(),
            &xform(5.0, 0.0, 5.0),
            &bb([4.0, -1.0, 4.0], [6.0, 1.0, 6.0]),
            &bb([4.0, -1.0, 4.0], [6.0, 1.0, 6.0]),
        );

        // Resolution through objectLookup_ answers the same handle+type
        // (equality ignores flags; both faces are checked separately).
        let located = provider.object(handle);
        assert_eq!(located, presence, "the handle resolves the entity object");
        assert_eq!(located.handle(), 42);
        assert!(located.is_type::<Entity>());
        assert!(located.flags().is_dynamic(), "flags ride with the object");

        // The spatial query answers the same scene object.
        let whole = bb([-20.0, -20.0, -20.0], [20.0, 20.0, 20.0]);
        let found = provider.query_region(&whole);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0], presence);

        // Movement keeps the mapping stable across the post-tick rebuild.
        provider.update_object(
            handle,
            &xform(-5.0, 0.0, -5.0),
            &bb([-6.0, -1.0, -6.0], [-4.0, 1.0, -4.0]),
            &bb([-6.0, -1.0, -6.0], [-4.0, 1.0, -4.0]),
        );
        provider.on_post_tick();
        assert_eq!(provider.object(handle), presence);

        // Removal unresolves the handle (the table stays valid; resolution
        // is an assert, as in the reference lookup chain).
        provider.remove_object(handle);
        assert!(provider.handle_is_valid(handle));
        assert_eq!(provider.num_objects(), 0);
        assert!(
            catch_unwind(AssertUnwindSafe(|| provider.object(handle))).is_err(),
            "a removed handle no longer resolves"
        );
    }
}
