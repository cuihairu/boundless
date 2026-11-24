//! BW-10-02-03 the dynamic scene provider: the server-slice port of
//! `lib/space/dynamic_scene_provider.{hpp,cpp}` — registering movable and
//! removable objects into a pair of dynamic loose octrees (collision and
//! visibility) and answering spatial queries over the visibility tree.
//!
//! Since BW-10-02-04 the stored objects are the real `scene::SceneObject`
//! typed handles (the reference `objects_` collection, read back through
//! `objectLookup_`). Since BW-10-02-05 the provider is a full
//! `SceneProvider`: the shared [`DynamicSceneFaces`] object exposes the
//! tick/collision/intersect faces the scene views collect, the tick view
//! listener drives the post-tick rebuild, and the object-change
//! notifications fire through the bound `ObjectChangeSceneView`.

use std::cell::RefCell;
use std::fmt;
use std::rc::{Rc, Weak};

use foundation::math::{BoundBox, ConvexHull, Matrix, Vector3};
use physics::{CollisionState, SweepParams, WorldTriangle};
use scene::{
    CollisionOperation, ICollisionSceneViewProvider, IIntersectSceneViewProvider,
    ITickSceneListener, ITickSceneProvider, IntersectionSet, ObjectChangeSceneView,
    ProviderSceneRef, RuntimeTypeID, SceneBoundFaces, SceneIntersectContext, SceneObject,
    SceneProvider, SceneToken, SceneViewRef, TickOperation, expose_collision_view,
    expose_intersect_view, expose_tick_view,
};

use crate::loose_octree::{DynamicLooseOctree, INVALID_NODE, NodeDataReference, NodeIndex};

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
#[derive(Clone)]
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
    /// `SceneProvider::pScene_`.
    scene_ref: ProviderSceneRef,
    /// The faces [`Scene::add_provider`] pushed at bind time
    /// (`pObjectChangeView_` plus the operations the reference fetches
    /// per call — the port fetches once at bind, same lazy singletons).
    bound_faces: Option<BoundFaces>,
    /// The shared view faces — the reference exposes `this`; the port
    /// exposes an object holding a `Weak` back-edge to the provider
    /// (no reference cycle).
    faces: Rc<DynamicSceneFaces>,
    /// The `TickSceneView::addListener(this)` face.
    tick_listener_face: Rc<RefCell<DynamicSceneTickListener>>,
}

/// The faces [`Scene::add_provider`] handed the provider at bind time.
/// The tick view itself is not stored — the provider reaches it through
/// the listener face the scene registered.
#[derive(Clone)]
struct BoundFaces {
    tick_operation: Rc<RefCell<TickOperation>>,
    collision_operation: Rc<RefCell<CollisionOperation>>,
    object_change_view: Rc<RefCell<ObjectChangeSceneView>>,
}

impl Default for DynamicSceneProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// The stored views/operations have no `Debug` faces; the summary reports
/// the collection state the reference logs would touch.
impl fmt::Debug for DynamicSceneProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DynamicSceneProvider")
            .field("num_objects", &self.objects.len())
            .field("num_dirty_objects", &self.num_dirty_objects)
            .field("scene", &self.scene_ref.scene())
            .field("bound", &self.bound_faces.is_some())
            .finish()
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
            scene_ref: ProviderSceneRef::default(),
            bound_faces: None,
            faces: Rc::new(DynamicSceneFaces {
                provider: Weak::new(),
            }),
            tick_listener_face: Rc::new(RefCell::new(DynamicSceneTickListener {
                provider: Weak::new(),
            })),
        }
    }

    /// Wrap the provider for scene registration and wire its shared faces
    /// to the new `Rc`. The reference exposes `this` as its view faces;
    /// the port's faces reach the provider through a `Weak` back-edge that
    /// only exists once the outer `Rc` does, so scene-bound providers must
    /// be constructed through here.
    pub fn shared() -> Rc<RefCell<Self>> {
        let provider = Rc::new(RefCell::new(Self::new()));
        let provider_weak = Rc::downgrade(&provider);
        {
            let mut bound = provider.borrow_mut();
            bound.faces = Rc::new(DynamicSceneFaces {
                provider: provider_weak.clone(),
            });
            bound.tick_listener_face = Rc::new(RefCell::new(DynamicSceneTickListener {
                provider: provider_weak,
            }));
        }
        provider
    }

    /// `setPartitionSizeHint` (`dynamic_scene_provider.cpp:498-501`).
    pub fn set_partition_size_hint(&mut self, partition_size_hint: f32) {
        self.partition_size_hint = partition_size_hint;
    }

    /// `addObject` (`dynamic_scene_provider.cpp:160-171`): insert, then
    /// `notifyObjectsAdded` through the bound view. Registered boundary:
    /// the reference would dereference a null `pObjectChangeView_` here
    /// when unbound; the port skips the notification instead.
    pub fn add_object(
        &mut self,
        object: SceneObject,
        world_xform: &Matrix,
        collision_bb: &BoundBox,
        visibility_bb: &BoundBox,
    ) -> DynamicObjectHandle {
        let registered = object.clone();
        let handle = self.insert_object_internal(object, world_xform, collision_bb, visibility_bb);

        if let Some(bound) = self.bound_faces.as_ref() {
            bound
                .object_change_view
                .borrow()
                .notify_objects_added(self.bound_scene_token(), &[registered]);
        }

        handle
    }

    /// `removeObject` (`dynamic_scene_provider.cpp:174-187`): an invalid
    /// handle is a no-op (the reference asserts then returns); the removal
    /// notification fires with the object resolved *before*
    /// `removeObject_internal` swaps it away (cpp:180-183). Same
    /// unbound-notification boundary as `add_object`.
    pub fn remove_object(&mut self, handle: DynamicObjectHandle) {
        if !self.dynamic_object_handles.is_valid(handle) {
            return;
        }

        let removed = self.objects[self.object_lookup[handle.index() as usize]].clone();
        if let Some(bound) = self.bound_faces.as_ref() {
            bound
                .object_change_view
                .borrow()
                .notify_objects_removed(self.bound_scene_token(), &[removed]);
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

        // Now push the list of changed objects out to listeners.
        // Note, the number of updated objects may not be the same as the
        // number reported, as some may have been removed since being
        // updated (`dynamic_scene_provider.cpp:391-399`).
        assert!(
            updated_objects.len() as u32 <= self.num_dirty_objects,
            "reference asserts updatedObjects <= numDirtyObjects"
        );
        if let Some(bound) = self.bound_faces.as_ref() {
            bound
                .object_change_view
                .borrow()
                .notify_objects_changed(self.bound_scene_token(), &updated_objects);
        }
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

    /// The announcing-provider identity the notifications carry. The
    /// reference passes `this` (`SceneProvider*`); the port carries the
    /// provider's bound scene token (registered adaptation). Only called
    /// on bound paths, where the scene is necessarily set.
    fn bound_scene_token(&self) -> SceneToken {
        self.scene_ref
            .scene()
            .expect("notifications fire on bound paths")
    }

    /// `Detail::collideReferences` (`dynamic_scene_provider.cpp:263-298`):
    /// each tree candidate resolved through
    /// `objects_[objectLookup_[index]]` and offered to its type handler
    /// until one stops the sweep. `object.isValid()` is answered by
    /// `SceneObject::is_valid` (`handle_ != 0`); tree contents are kept
    /// exact by `remove_object_node`, so the lookup itself never misses.
    /// The two reference overloads arrive as the `dispatch` closure.
    fn collide_references(
        &self,
        state: &mut CollisionState,
        node_references: &[NodeDataReference],
        dispatch: &mut dyn FnMut(&SceneObject, &mut CollisionState) -> bool,
    ) -> bool {
        assert!(
            self.scene_ref.scene().is_some(),
            "reference asserts a bound scene in collideReferences"
        );

        for node_ref in node_references.iter().copied() {
            for handle in self.collision_tree_contents[node_ref as usize]
                .iter()
                .copied()
            {
                let object = &self.objects[self.object_lookup[handle.index() as usize]];
                if object.is_valid() && dispatch(object, state) {
                    return true;
                }
            }
        }

        false
    }

    /// The `intersect` face body
    /// (`dynamic_scene_provider.cpp:449-496`): hull walk over the
    /// visibility tree, per-object visibility-bound filter, insertion into
    /// the set.
    fn intersect_hull_context(
        &self,
        context: &SceneIntersectContext,
        hull: &ConvexHull,
        intersection: &mut IntersectionSet,
    ) -> usize {
        // Make sure these objects are contextually interesting.
        if !context.include_dynamic_objects() {
            return 0;
        }

        let mut num_intersected = 0usize;
        for node_ref in self.visibility_tree.intersect_hull(hull) {
            for handle in self.visibility_tree_contents[node_ref as usize]
                .iter()
                .copied()
            {
                let bb = &self.object_visibility_bbs[handle.index() as usize];

                if bb.inside_out() {
                    continue;
                }

                if hull.intersects_box(bb) {
                    let slot = self.object_lookup[handle.index() as usize];
                    intersection.insert(self.objects[slot].clone());
                    num_intersected += 1;
                }
            }
        }

        // Make sure we aren't returning more objects than actually exist
        // in the provider (cpp:492).
        assert!(
            num_intersected <= self.objects.len(),
            "reference asserts numIntersected <= objects_.size()"
        );
        num_intersected
    }
}

/// The shared view faces (`dynamic_scene_provider.hpp` exposes `this`
/// under three interfaces; the port wraps the provider's `Weak` so the
/// faces can be handed to the views without a reference cycle). A face on
/// a dropped provider answers quietly — the reference would leave a
/// dangling pointer in the view (registered adaptation).
struct DynamicSceneFaces {
    provider: Weak<RefCell<DynamicSceneProvider>>,
}

impl DynamicSceneFaces {
    /// Upgrade or answer nothing, mirroring the reference's implicit
    /// "provider is alive" precondition.
    fn upgrade(&self) -> Option<Rc<RefCell<DynamicSceneProvider>>> {
        self.provider.upgrade()
    }
}

/// `ITickSceneProvider` (`dynamic_scene_provider.cpp:129-137` exposure +
/// cpp:304-318 tick, cpp:405-419 updateAnimations).
impl ITickSceneProvider for DynamicSceneFaces {
    fn tick(&self, d_time: f32) {
        let provider = match self.upgrade() {
            Some(provider) => provider,
            None => return,
        };
        let mut provider = provider.borrow_mut();

        if provider.objects.is_empty() {
            return;
        }
        assert!(
            provider.scene_ref.scene().is_some(),
            "reference asserts a bound scene when ticking"
        );
        let tick_operation = provider
            .bound_faces
            .as_ref()
            .expect("reference asserts the tick operation exists")
            .tick_operation
            .clone();
        tick_operation
            .borrow_mut()
            .tick(&mut provider.objects, d_time);
    }

    fn update_animations(&self, d_time: f32) {
        let provider = match self.upgrade() {
            Some(provider) => provider,
            None => return,
        };
        let mut provider = provider.borrow_mut();

        if provider.objects.is_empty() {
            return;
        }
        assert!(
            provider.scene_ref.scene().is_some(),
            "reference asserts a bound scene when animating"
        );
        let tick_operation = provider
            .bound_faces
            .as_ref()
            .expect("reference asserts the tick operation exists")
            .tick_operation
            .clone();
        tick_operation
            .borrow_mut()
            .update_animations(&mut provider.objects, d_time);
    }
}

/// `ICollisionSceneViewProvider` (`dynamic_scene_provider.cpp:421-446`):
/// ray sweeps walk the segment over the collision tree, prism sweeps the
/// shape bound; both then offer the candidates to the bound
/// `CollisionOperation`.
impl ICollisionSceneViewProvider for DynamicSceneFaces {
    fn collide_ray(
        &self,
        source: &Vector3,
        extent: &Vector3,
        sp: &SweepParams,
        state: &mut CollisionState,
    ) -> bool {
        let provider = match self.upgrade() {
            Some(provider) => provider,
            None => return false,
        };
        let provider = provider.borrow();

        let node_references = provider
            .collision_tree
            .intersect_ray_segment(source, extent);
        let bound = provider
            .bound_faces
            .as_ref()
            .expect("reference asserts the collision operation exists");
        let operation = bound.collision_operation.borrow();
        provider.collide_references(state, &node_references, &mut |object, state| {
            operation.collide_ray(object, source, extent, sp, state)
        })
    }

    fn collide_prism(
        &self,
        source: &WorldTriangle,
        extent: &Vector3,
        sp: &SweepParams,
        state: &mut CollisionState,
    ) -> bool {
        let provider = match self.upgrade() {
            Some(provider) => provider,
            None => return false,
        };
        let provider = provider.borrow();

        let node_references = provider.collision_tree.intersect_aabb(sp.shape_box());
        let bound = provider
            .bound_faces
            .as_ref()
            .expect("reference asserts the collision operation exists");
        let operation = bound.collision_operation.borrow();
        provider.collide_references(state, &node_references, &mut |object, state| {
            operation.collide_prism(object, source, extent, sp, state)
        })
    }
}

/// `IIntersectSceneViewProvider` (`dynamic_scene_provider.cpp:449-496`).
impl IIntersectSceneViewProvider for DynamicSceneFaces {
    fn intersect(
        &self,
        context: &SceneIntersectContext,
        hull: &ConvexHull,
        intersection: &mut IntersectionSet,
    ) -> usize {
        match self.upgrade() {
            Some(provider) => provider
                .borrow()
                .intersect_hull_context(context, hull, intersection),
            None => 0,
        }
    }
}

/// The `TickSceneView::addListener(this)` face
/// (`dynamic_scene_provider.cpp:152-154`): pre-tick does nothing
/// (cpp:321-324), post-tick is the deferred rebuild.
struct DynamicSceneTickListener {
    provider: Weak<RefCell<DynamicSceneProvider>>,
}

impl ITickSceneListener for DynamicSceneTickListener {
    fn on_pre_tick(&mut self) {
        // Nothing to be done.
    }

    fn on_post_tick(&mut self) {
        if let Some(provider) = self.provider.upgrade() {
            provider.borrow_mut().on_post_tick();
        }
    }
}

/// `SceneProvider` — the provider keeps its scene binding in
/// [`ProviderSceneRef`], hands out the shared faces under the three view
/// ids (`getView`, cpp:128-141) and drops the bound faces on unbind
/// (the `pObjectChangeView_ = NULL` half of `onSetScene`, cpp:143-157;
/// the listener removal itself is performed by the scene).
impl SceneProvider for DynamicSceneProvider {
    fn scene(&self) -> Option<SceneToken> {
        self.scene_ref.scene()
    }

    fn set_scene(&mut self, scene: Option<SceneToken>) {
        let old_scene = self.scene_ref.replace(scene);
        self.on_set_scene(old_scene, scene);
    }

    fn on_set_scene(&mut self, _old_scene: Option<SceneToken>, new_scene: Option<SceneToken>) {
        if new_scene.is_none() {
            // pObjectChangeView_ = NULL (and the stored operations with
            // it; the tick listener is removed by the scene).
            self.bound_faces = None;
        }
    }

    fn get_view(&self, type_id: RuntimeTypeID) -> Option<SceneViewRef> {
        let mut result = None;
        // The reference expose chain: tick, then collision, then intersect.
        let tick_face: Rc<dyn ITickSceneProvider> = self.faces.clone();
        expose_tick_view(tick_face, type_id, &mut result);
        let collision_face: Rc<dyn ICollisionSceneViewProvider> = self.faces.clone();
        expose_collision_view(collision_face, type_id, &mut result);
        let intersect_face: Rc<dyn IIntersectSceneViewProvider> = self.faces.clone();
        expose_intersect_view(intersect_face, type_id, &mut result);
        result
    }

    fn tick_listener(&self) -> Option<Rc<RefCell<dyn ITickSceneListener>>> {
        Some(self.tick_listener_face.clone())
    }

    fn on_scene_bound(&mut self, faces: &SceneBoundFaces) {
        self.bound_faces = Some(BoundFaces {
            tick_operation: faces.tick_operation.clone(),
            collision_operation: faces.collision_operation.clone(),
            object_change_view: faces.object_change_view.clone(),
        });
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
    use foundation::math::{PlaneEq, RayIntersectionType};
    use physics::{CollisionCallback, SweepShape};
    use scene::SceneObjectFlags;
    use scene::{
        CollisionOperation, CollisionSceneView, ICollisionTypeHandler, IntersectSceneView,
        IntersectionSet, ObjectChangeArgs, ObjectChangeDelegate, ObjectChangeSceneView, Scene,
        SceneIntersectContext, SceneProvider, SceneViewRef, TickSceneView,
        collision_provider_view_id, intersect_provider_view_id, tick_provider_view_id,
    };

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

    /// The six-plane box hull the reference intersect callers build
    /// (interior = the negative half-spaces).
    fn box_hull(min: [f32; 3], max: [f32; 3]) -> ConvexHull {
        ConvexHull::new(vec![
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), max[0]),
            PlaneEq::new(Vector3::new(-1.0, 0.0, 0.0), -min[0]),
            PlaneEq::new(Vector3::new(0.0, 1.0, 0.0), max[1]),
            PlaneEq::new(Vector3::new(0.0, -1.0, 0.0), -min[1]),
            PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), max[2]),
            PlaneEq::new(Vector3::new(0.0, 0.0, -1.0), -min[2]),
        ])
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

    /// Object-change recorder: one delegate per list, tagged so a single
    /// shared log can hold all three notification kinds.
    struct ChangeLog {
        tag: char,
        entries: Rc<RefCell<Vec<(char, u64)>>>,
    }

    impl ObjectChangeDelegate for ChangeLog {
        fn on_objects(&self, _scene: Option<scene::SceneToken>, args: &ObjectChangeArgs) {
            for object in args.objects {
                self.entries.borrow_mut().push((self.tag, object.handle()));
            }
        }
    }

    /// Sweep handler clipping a ray against the registered entity's
    /// collision bound — the type-dispatch stand-in for a real chunk
    /// provider handler.
    struct ClipHandler {
        bb: BoundBox,
    }

    impl ICollisionTypeHandler for ClipHandler {
        fn do_collide_ray(
            &self,
            _object: &SceneObject,
            source: &Vector3,
            extent: &Vector3,
            _sp: &SweepParams,
            state: &mut CollisionState,
        ) -> bool {
            let mut coord = Vector3::ZERO;
            match self.bb.intersects_ray_hit(source, extent, &mut coord) {
                RayIntersectionType::OriginInside => {
                    state.dist = 0.0;
                    true
                }
                RayIntersectionType::Intersection => {
                    state.dist = (coord - *source).length() / (*extent - *source).length();
                    true
                }
                RayIntersectionType::NoIntersection => false,
            }
        }

        fn do_collide_prism(
            &self,
            _object: &SceneObject,
            _source: &WorldTriangle,
            _extent: &Vector3,
            _sp: &SweepParams,
            _state: &mut CollisionState,
        ) -> bool {
            false
        }
    }

    struct NopCallback;
    impl CollisionCallback for NopCallback {}

    /// The shared provider answers all three view ids with the right
    /// erased variant (`getView`, cpp:128-141).
    #[test]
    fn a_shared_provider_exposes_the_tick_collision_and_intersect_faces() {
        let provider = DynamicSceneProvider::shared();
        let as_provider: Rc<RefCell<dyn SceneProvider>> = provider.clone();

        let asked = as_provider.borrow().get_view(tick_provider_view_id());
        assert!(matches!(asked, Some(SceneViewRef::Tick(_))));
        let asked = as_provider.borrow().get_view(collision_provider_view_id());
        assert!(matches!(asked, Some(SceneViewRef::Collision(_))));
        let asked = as_provider.borrow().get_view(intersect_provider_view_id());
        assert!(matches!(asked, Some(SceneViewRef::Intersect(_))));
    }

    /// Acceptance: 同一编译空间两种消费者均可加载 — one scene, one dynamic
    /// provider; both the collision and the intersect views load over the
    /// same objects, the tick view drives the post-tick rebuild through
    /// the listener, and the object-change notifications flow.
    #[test]
    fn one_compiled_space_serves_the_collision_and_intersect_consumers() {
        let mut space = Scene::new();
        let provider = DynamicSceneProvider::shared();
        let as_provider: Rc<RefCell<dyn SceneProvider>> = provider.clone();
        space.add_provider(as_provider.clone());

        // Both consumers load and each collects exactly the provider face.
        let collision_view = space.get_view::<CollisionSceneView>();
        let intersect_view = space.get_view::<IntersectSceneView>();
        assert_eq!(collision_view.borrow().providers_len(), 1);
        assert_eq!(intersect_view.borrow().providers_len(), 1);

        // The object-change view is part of the same space; the recorders
        // watch all three notification lists.
        let object_change_view = space.get_view::<ObjectChangeSceneView>();
        let log = Rc::new(RefCell::new(Vec::new()));
        object_change_view
            .borrow_mut()
            .objects_added()
            .push(Box::new(ChangeLog {
                tag: 'a',
                entries: log.clone(),
            }));
        object_change_view
            .borrow_mut()
            .objects_removed()
            .push(Box::new(ChangeLog {
                tag: 'r',
                entries: log.clone(),
            }));
        object_change_view
            .borrow_mut()
            .objects_changed()
            .push(Box::new(ChangeLog {
                tag: 'c',
                entries: log.clone(),
            }));

        // The entity registers into the bound space and the added
        // notification flows through the view.
        let mut flags = SceneObjectFlags::new();
        flags.set_is_dynamic(true);
        let presence = SceneObject::typed::<Entity>(42, flags);
        let collision_bb = bb([-1.0, -1.0, 4.0], [1.0, 1.0, 6.0]);
        let handle = provider.borrow_mut().add_object(
            presence.clone(),
            &xform(0.0, 0.0, 5.0),
            &collision_bb,
            &collision_bb,
        );
        assert_eq!(*log.borrow(), [('a', 42)]);

        // The collision handler is registered on the space's
        // CollisionOperation — the same lazy singleton the provider
        // stored at bind time.
        let operation = space.get_object_operation::<CollisionOperation>();
        operation
            .borrow_mut()
            .base_mut()
            .add_handler_for::<Entity>(Box::new(ClipHandler { bb: collision_bb }));

        // Consumer 1 — the collision view sweeps through the provider and
        // stops at the entity's bound (half way along the segment).
        let dist = collision_view.borrow().collide_ray(
            &Vector3::ZERO,
            &Vector3::new(0.0, 0.0, 10.0),
            &mut NopCallback,
        );
        assert!((dist - 0.4).abs() < 1e-6, "hit at z=4 of 10: {dist}");
        // The prism face lets the sweep run on: the initial dist answers.
        let triangle = WorldTriangle::new(
            Vector3::new(-1.0, -1.0, 0.0),
            Vector3::new(1.0, -1.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            0,
        );
        let prism_dist = collision_view.borrow().collide_prism(
            &triangle,
            &Vector3::new(0.0, 0.0, 4.0),
            &mut NopCallback,
        );
        assert_eq!(prism_dist, -1.0);
        // A sweep starting inside the bound answers dist 0 (origin inside);
        // a sweep missing it entirely answers the untouched initial dist.
        let inside = collision_view.borrow().collide_ray(
            &Vector3::new(0.0, 0.0, 5.0),
            &Vector3::new(0.0, 0.0, 10.0),
            &mut NopCallback,
        );
        assert_eq!(inside, 0.0);
        let miss = collision_view.borrow().collide_ray(
            &Vector3::new(5.0, 0.0, 0.0),
            &Vector3::new(5.0, 0.0, 10.0),
            &mut NopCallback,
        );
        assert_eq!(miss, -1.0);

        // Consumer 2 — the intersect view answers the hull query over the
        // same objects (the hull wraps the entity's visibility bound).
        let hull = ConvexHull::new(vec![
            PlaneEq::new(Vector3::new(1.0, 0.0, 0.0), 2.0),
            PlaneEq::new(Vector3::new(-1.0, 0.0, 0.0), 2.0),
            PlaneEq::new(Vector3::new(0.0, 1.0, 0.0), 2.0),
            PlaneEq::new(Vector3::new(0.0, -1.0, 0.0), 2.0),
            PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 8.0),
            PlaneEq::new(Vector3::new(0.0, 0.0, -1.0), -3.0),
        ]);
        let context = SceneIntersectContext::new(space.token().into());
        let mut intersection = IntersectionSet::new();
        let count = intersect_view
            .borrow()
            .intersect(&context, &hull, &mut intersection);
        assert_eq!(count, 1);
        assert_eq!(
            intersection
                .objects_of_type(SceneObject::type_of::<Entity>())
                .len(),
            1
        );

        // Dynamics excluded from the context: the provider face answers 0
        // before walking anything (cpp:455-457).
        let mut statics_only = SceneIntersectContext::new(space.token().into());
        statics_only.set_include_dynamic_objects(false);
        let mut empty_set = IntersectionSet::new();
        let none = intersect_view
            .borrow()
            .intersect(&statics_only, &hull, &mut empty_set);
        assert_eq!(none, 0);
        assert_eq!(empty_set.num_objects(), 0);

        // The tick view drives provider tick and the post-tick rebuild
        // through the listener; the changed notification carries the moved
        // entity and queries answer the new position.
        provider.borrow_mut().update_object(
            handle,
            &xform(0.0, 0.0, -5.0),
            &bb([-1.0, -1.0, -6.0], [1.0, 1.0, -4.0]),
            &bb([-1.0, -1.0, -6.0], [1.0, 1.0, -4.0]),
        );
        // Re-staging before the rebuild counts the object once (the dirty
        // flag guards the increment, cpp:611-617).
        provider.borrow_mut().update_object(
            handle,
            &xform(0.0, 0.0, -5.0),
            &bb([-1.0, -1.0, -6.0], [1.0, 1.0, -4.0]),
            &bb([-1.0, -1.0, -6.0], [1.0, 1.0, -4.0]),
        );
        assert_eq!(
            provider.borrow().num_dirty_objects,
            1,
            "a re-staged object counts once"
        );
        let tick_view = space.get_view::<TickSceneView>();
        tick_view.borrow_mut().tick(0.1);
        assert_eq!(*log.borrow(), [('a', 42), ('c', 42)]);

        // The animation pass runs over the same slice without a rebuild
        // notification (updateAnimations, cpp:405-419).
        tick_view.borrow_mut().update_animations(0.05);
        assert_eq!(
            *log.borrow(),
            [('a', 42), ('c', 42)],
            "animating does not notify"
        );

        let old_spot = provider
            .borrow()
            .query_region(&bb([-2.0, -2.0, 3.0], [2.0, 2.0, 7.0]));
        assert!(old_spot.is_empty(), "the entity moved away");
        let new_spot = provider
            .borrow()
            .query_region(&bb([-2.0, -2.0, -7.0], [2.0, 2.0, -3.0]));
        assert_eq!(new_spot.len(), 1);
        assert_eq!(new_spot[0], presence);

        // Removal notifies through the same view.
        provider.borrow_mut().remove_object(handle);
        assert_eq!(*log.borrow(), [('a', 42), ('c', 42), ('r', 42)]);
    }

    /// Unbinding drops the stored faces: later notifications are skipped
    /// (the reference would fire through a null view) and queries keep
    /// answering from the trees (registered boundary).
    #[test]
    fn unbinding_drops_the_faces_and_skips_the_notifications() {
        let mut space = Scene::new();
        let provider = DynamicSceneProvider::shared();
        let as_provider: Rc<RefCell<dyn SceneProvider>> = provider.clone();
        space.add_provider(as_provider.clone());

        let object_change_view = space.get_view::<ObjectChangeSceneView>();
        let log = Rc::new(RefCell::new(Vec::new()));
        object_change_view
            .borrow_mut()
            .objects_added()
            .push(Box::new(ChangeLog {
                tag: 'a',
                entries: log.clone(),
            }));

        let presence = SceneObject::typed::<Entity>(7, SceneObjectFlags::new());
        let handle = provider.borrow_mut().add_object(
            presence.clone(),
            &xform(0.0, 0.0, 0.0),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
        );
        assert_eq!(*log.borrow(), [('a', 7)], "bound: the add notifies");

        space.remove_provider(&as_provider);

        provider.borrow_mut().update_object(
            handle,
            &xform(1.0, 0.0, 0.0),
            &bb([0.0, -1.0, -1.0], [2.0, 1.0, 1.0]),
            &bb([0.0, -1.0, -1.0], [2.0, 1.0, 1.0]),
        );
        provider.borrow_mut().remove_object(handle);
        provider.borrow_mut().on_post_tick();
        assert_eq!(*log.borrow(), [('a', 7)], "unbound: nothing notifies");
    }

    /// The reference MF_ASSERTs on the bound faces before colliding; the
    /// port answers the same assert when the face drives an unbound
    /// provider.
    #[test]
    #[should_panic(expected = "reference asserts the collision operation exists")]
    fn colliding_through_the_faces_without_a_bound_scene_asserts() {
        let mut space = Scene::new();
        let provider = DynamicSceneProvider::shared();
        let as_provider: Rc<RefCell<dyn SceneProvider>> = provider.clone();
        space.add_provider(as_provider.clone());

        provider.borrow_mut().add_object(
            SceneObject::typed::<Entity>(1, SceneObjectFlags::new()),
            &xform(0.0, 0.0, 0.0),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
        );

        let face = match provider.borrow().get_view(collision_provider_view_id()) {
            Some(SceneViewRef::Collision(face)) => face,
            _ => unreachable!(),
        };
        space.remove_provider(&as_provider);

        let sp = SweepParams::new(
            &SweepShape::Ray(Vector3::ZERO),
            Vector3::new(0.0, 0.0, 10.0),
        );
        let mut cb = NopCallback;
        let mut state = CollisionState::new(&mut cb);
        face.collide_ray(
            &Vector3::ZERO,
            &Vector3::new(0.0, 0.0, 10.0),
            &sp,
            &mut state,
        );
    }

    /// Same assert family for the tick face.
    #[test]
    #[should_panic(expected = "reference asserts a bound scene when ticking")]
    fn ticking_through_the_faces_without_a_bound_scene_asserts() {
        let mut space = Scene::new();
        let provider = DynamicSceneProvider::shared();
        let as_provider: Rc<RefCell<dyn SceneProvider>> = provider.clone();
        space.add_provider(as_provider.clone());

        provider.borrow_mut().add_object(
            SceneObject::typed::<Entity>(1, SceneObjectFlags::new()),
            &xform(0.0, 0.0, 0.0),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
        );

        let face = match provider.borrow().get_view(tick_provider_view_id()) {
            Some(SceneViewRef::Tick(face)) => face,
            _ => unreachable!(),
        };
        space.remove_provider(&as_provider);

        face.tick(0.1);
    }

    /// The faces hold only Weak back-edges: once the provider Rc is gone
    /// every face answers quietly (the reference would leave dangling
    /// provider pointers in the views — registered adaptation).
    #[test]
    fn the_faces_answer_quietly_once_the_provider_is_dropped() {
        let mut space = Scene::new();
        let provider = DynamicSceneProvider::shared();
        let as_provider: Rc<RefCell<dyn SceneProvider>> = provider.clone();
        space.add_provider(as_provider.clone());

        // Grab every face (and the tick listener) while the provider lives.
        let tick_face = match provider.borrow().get_view(tick_provider_view_id()) {
            Some(SceneViewRef::Tick(face)) => face,
            _ => unreachable!(),
        };
        let collision_face = match provider.borrow().get_view(collision_provider_view_id()) {
            Some(SceneViewRef::Collision(face)) => face,
            _ => unreachable!(),
        };
        let intersect_face = match provider.borrow().get_view(intersect_provider_view_id()) {
            Some(SceneViewRef::Intersect(face)) => face,
            _ => unreachable!(),
        };
        let listener = provider
            .borrow()
            .tick_listener()
            .expect("the shared provider always hands out its tick listener");

        // The scene lets go, then the test does too.
        space.remove_provider(&as_provider);
        drop(provider);
        drop(as_provider);

        // The sweeps answer no-hit, the hull query answers 0, and the
        // tick faces return before any assert.
        let ray_sp = SweepParams::new(
            &SweepShape::Ray(Vector3::ZERO),
            Vector3::new(0.0, 0.0, 10.0),
        );
        let mut cb = NopCallback;
        let mut state = CollisionState::new(&mut cb);
        assert!(!collision_face.collide_ray(
            &Vector3::ZERO,
            &Vector3::new(0.0, 0.0, 10.0),
            &ray_sp,
            &mut state
        ));
        let triangle = WorldTriangle::new(
            Vector3::new(-1.0, -1.0, 0.0),
            Vector3::new(1.0, -1.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            0,
        );
        let prism_sp = SweepParams::new(&SweepShape::Prism(triangle), Vector3::new(0.0, 0.0, 4.0));
        assert!(!collision_face.collide_prism(
            &triangle,
            &Vector3::new(0.0, 0.0, 4.0),
            &prism_sp,
            &mut state
        ));

        let context = SceneIntersectContext::new(None);
        let mut set = IntersectionSet::new();
        assert_eq!(
            intersect_face.intersect(&context, &box_hull([-20.0; 3], [20.0; 3]), &mut set),
            0
        );

        tick_face.tick(0.1);
        tick_face.update_animations(0.05);
        listener.borrow_mut().on_post_tick();
    }

    /// Nothing registered and no objects: both tick faces answer before
    /// the reference's bound-scene assert can fire.
    #[test]
    fn an_empty_provider_ticks_and_animates_before_any_scene_assert() {
        let provider = DynamicSceneProvider::shared();
        let tick_face = match provider.borrow().get_view(tick_provider_view_id()) {
            Some(SceneViewRef::Tick(face)) => face,
            _ => unreachable!(),
        };

        tick_face.tick(0.1);
        tick_face.update_animations(0.05);
    }

    /// The query filters defend against a visibility bound gone
    /// inside-out while the handle is still listed in the node contents,
    /// and removal walks cleanly past a node slot the object never got
    /// (inside-out collision bound).
    #[test]
    fn queries_and_removals_defend_against_degenerate_bounds() {
        let mut provider = DynamicSceneProvider::new();

        // A valid visibility bound with an inside-out collision bound:
        // the collision node slot stays INVALID_NODE and the removal
        // early-outs on it.
        let collision_free = provider.add_object(
            entity(7),
            &xform(0.0, 0.0, 0.0),
            &BoundBox::default(),
            &bb([-1.0, -1.0, -1.0], [1.0, 1.0, 1.0]),
        );
        let whole = bb([-20.0, -20.0, -20.0], [20.0, 20.0, 20.0]);
        assert_eq!(entity_ids(&provider, &whole), [7]);

        // A valid registration whose visibility bound then goes
        // inside-out in place: the handle stays in the node contents and
        // the queries must skip it (the reference transient between a
        // staged update and the post-tick rebuild).
        let stale = provider.add_object(
            entity(8),
            &xform(0.0, 0.0, 5.0),
            &bb([-1.0, -1.0, 4.0], [1.0, 1.0, 6.0]),
            &bb([-1.0, -1.0, 4.0], [1.0, 1.0, 6.0]),
        );
        let index = stale.index() as usize;
        provider.object_visibility_bbs[index] = BoundBox::default();
        assert_eq!(
            entity_ids(&provider, &whole),
            [7],
            "the stale inside-out bound is skipped, the live object answers"
        );

        let context = SceneIntersectContext::new(None);
        let mut set = IntersectionSet::new();
        assert_eq!(
            provider.intersect_hull_context(&context, &box_hull([-20.0; 3], [20.0; 3]), &mut set),
            1
        );
        assert_eq!(
            set.objects_of_type(SceneObject::type_of::<Entity>())
                .iter()
                .map(SceneObject::handle)
                .collect::<Vec<_>>(),
            [7],
            "the stale inside-out bound stays out of the set"
        );

        // Both removals walk their trees; the swap-erase keeps the rest.
        provider.remove_object(stale);
        provider.remove_object(collision_free);
        provider.on_post_tick();
        assert_eq!(provider.num_objects(), 0);
        assert!(provider.query_region(&whole).is_empty());
    }

    /// A visited node answers only the objects the query actually
    /// overlaps: the neighbours sharing the leaf are filtered per bound.
    #[test]
    fn a_visited_node_answers_only_the_objects_the_query_overlaps() {
        let mut provider = DynamicSceneProvider::new();
        provider.set_partition_size_hint(20.0);
        provider.resize_scene(&bb([-40.0, -40.0, -40.0], [40.0, 40.0, 40.0]));

        // Two neighbours land in the same leaf: identical x/y centres,
        // z centres in the same 20m cell.
        provider.add_object(
            entity(1),
            &xform(0.0, 0.0, 5.0),
            &bb([-1.0, -1.0, 4.0], [1.0, 1.0, 6.0]),
            &bb([-1.0, -1.0, 4.0], [1.0, 1.0, 6.0]),
        );
        provider.add_object(
            entity(2),
            &xform(0.0, 0.0, 5.6),
            &bb([-1.0, -1.0, 4.6], [1.0, 1.0, 6.6]),
            &bb([-1.0, -1.0, 4.6], [1.0, 1.0, 6.6]),
        );

        let both = bb([-2.0, -2.0, 4.0], [2.0, 2.0, 7.0]);
        assert_eq!(entity_ids(&provider, &both), [1, 2]);

        // The region touches only object 2's bound but still overlaps the
        // shared leaf.
        let upper = bb([-1.0, -1.0, 6.1], [1.0, 1.0, 7.5]);
        assert_eq!(entity_ids(&provider, &upper), [2]);

        // The hull query over the same slab skips object 1's bound too.
        let context = SceneIntersectContext::new(None);
        let mut set = IntersectionSet::new();
        let count = provider.intersect_hull_context(
            &context,
            &box_hull([-1.0, -1.0, 6.1], [1.0, 1.0, 7.5]),
            &mut set,
        );
        assert_eq!(count, 1);
        assert_eq!(
            set.objects_of_type(SceneObject::type_of::<Entity>())
                .iter()
                .map(SceneObject::handle)
                .collect::<Vec<_>>(),
            [2]
        );
    }

    /// The Default and Debug faces report the provider summary the
    /// reference's logging would touch.
    #[test]
    fn debug_and_default_faces_report_the_provider_summary() {
        let table = HandleTable::default();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);

        let provider = DynamicSceneProvider::default();
        assert_eq!(
            format!("{provider:?}"),
            "DynamicSceneProvider { num_objects: 0, num_dirty_objects: 0, scene: None, bound: false }"
        );

        let mut space = Scene::new();
        let shared = DynamicSceneProvider::shared();
        let as_provider: Rc<RefCell<dyn SceneProvider>> = shared.clone();
        space.add_provider(as_provider);
        let summary = format!("{:?}", shared.borrow());
        assert!(summary.contains("scene: Some("), "{summary}");
        assert!(summary.contains("bound: true"), "{summary}");
    }
}
