//! BW-08-01-04: mount and unmount a physical Space.
//!
//! A space's geometry lives behind [`PhysicalChunkSpace`]
//! (`physical_chunk_space.hpp`), an [`IPhysicalSpace`] that mounts resource
//! mappings into a chunk space: `loadResource` (`physical_chunk_space.cpp:82`)
//! asks the underlying `ChunkSpace::addMappingAsync` for an asynchronous
//! mount, `unloadResource` (`:96`) removes the mapping again, `clear` (`:186`)
//! unloads everything, and `reuse` (`:177`) is `clear`. Chunks themselves are
//! shared, reference-counted objects (`Chunk`, `chunk.hpp`): every mapping
//! holds one reference, and the chunk is destroyed when its last reference is
//! released. That is this task's acceptance, "引用在销毁后释放" — drop the
//! space, and every reference it held goes with it; the chunk survives only
//! while some other holder remains.
//!
//! The delegated flavour, [`PhysicalDelegateSpace`], records mappings but
//! holds no chunks and always reports fully unloaded.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use crate::space::SpaceId;
use crate::space_data::SpaceEntryId;

/// A resource path, the identity of a chunk (`BW::string path`,
/// `physical_chunk_space.cpp:82`).
pub type ChunkPath = String;

/// The chunk store (`ChunkCache`): the chunks that mappings mount. A chunk is
/// created by the first mapping that mounts it and destroyed when the last
/// reference to it is released.
#[derive(Debug, Default)]
pub struct ChunkStore {
    chunks: BTreeMap<ChunkPath, usize>,
}

impl ChunkStore {
    /// An empty store.
    pub fn new() -> Self {
        ChunkStore::default()
    }

    /// `Chunk::incRef` (`chunk.hpp`): mount one reference, creating the chunk
    /// if this is the first. Returns the new reference count.
    pub fn acquire(&mut self, path: &str) -> usize {
        let refs = self.chunks.entry(path.to_string()).or_insert(0);
        *refs += 1;
        *refs
    }

    /// `Chunk::decRef`: release one reference. Returns whether the chunk was
    /// destroyed (`if (pChunk_->decRef()) delete pChunk_`).
    pub fn release(&mut self, path: &str) -> bool {
        match self.chunks.get_mut(path) {
            Some(refs) if *refs > 1 => {
                *refs -= 1;
                false
            }
            Some(_) => {
                self.chunks.remove(path);
                true
            }
            None => false,
        }
    }

    /// The reference count of a chunk, `0` when it is not held.
    pub fn ref_count(&self, path: &str) -> usize {
        self.chunks.get(path).copied().unwrap_or(0)
    }

    /// Whether a chunk is still alive in the store.
    pub fn is_live(&self, path: &str) -> bool {
        self.chunks.contains_key(path)
    }

    /// How many chunks the store holds.
    pub fn num_chunks(&self) -> usize {
        self.chunks.len()
    }
}

/// One mounted mapping of a chunk space (`ChunkSpace::Mapping`): a handle to
/// the store that releases its chunk when dropped.
#[derive(Debug)]
pub struct ChunkRef {
    store: Rc<RefCell<ChunkStore>>,
    path: ChunkPath,
}

impl ChunkRef {
    /// The chunk this mapping mounted.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The chunk's current reference count in the store.
    pub fn refs(&self) -> usize {
        self.store.borrow().ref_count(&self.path)
    }
}

impl Drop for ChunkRef {
    /// Release the reference this mapping held (`Chunk::decRef`).
    fn drop(&mut self) {
        self.store.borrow_mut().release(&self.path);
    }
}

/// `PhysicalChunkSpace` (`physical_chunk_space.hpp`): the chunk-backed
/// physical space of one space. Its mappings mount chunks into a shared
/// store; dropping the space releases them all.
#[derive(Debug)]
pub struct PhysicalChunkSpace {
    /// The id of the space this physical space serves.
    space_id: SpaceId,
    /// The shared chunk store (`pChunkSpace_`).
    store: Rc<RefCell<ChunkStore>>,
    /// The completed mappings, `SpaceEntryID -> mounted chunk`
    /// (`ChunkSpace`'s mappings_).
    mappings: BTreeMap<SpaceEntryId, ChunkRef>,
    /// The mappings still waiting for their asynchronous load
    /// (`addMappingAsync` / `isMappingPending`).
    pending: BTreeSet<SpaceEntryId>,
    /// The resource path each pending mapping is loading, taken off the
    /// request when it was made.
    pending_paths: BTreeMap<SpaceEntryId, ChunkPath>,
}

impl PhysicalChunkSpace {
    /// A fresh physical chunk space (`PhysicalChunkSpace::PhysicalChunkSpace`,
    /// `physical_chunk_space.cpp:63`).
    pub fn new(space_id: SpaceId) -> Self {
        PhysicalChunkSpace {
            space_id,
            store: Rc::new(RefCell::new(ChunkStore::new())),
            mappings: BTreeMap::new(),
            pending: BTreeSet::new(),
            pending_paths: BTreeMap::new(),
        }
    }

    /// A physical chunk space over a store other spaces already share, so
    /// their mappings can hold the same chunks.
    pub fn shared(space_id: SpaceId, store: Rc<RefCell<ChunkStore>>) -> Self {
        PhysicalChunkSpace {
            space_id,
            store,
            mappings: BTreeMap::new(),
            pending: BTreeSet::new(),
            pending_paths: BTreeMap::new(),
        }
    }

    /// The space this physical space serves.
    pub fn space_id(&self) -> SpaceId {
        self.space_id
    }

    /// The number of completed mappings (`ChunkSpace::numMappings`).
    pub fn num_mappings(&self) -> usize {
        self.mappings.len()
    }

    /// The mapping of one entry, once its load has completed.
    pub fn mapping(&self, entry: SpaceEntryId) -> Option<&ChunkRef> {
        self.mappings.get(&entry)
    }

    /// `ChunkSpace::isMappingPending`: some mappings are still loading.
    pub fn is_mapping_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// `PhysicalChunkSpace::loadResource` (`physical_chunk_space.cpp:82`):
    /// request an asynchronous mount. The chunk is not touched until the load
    /// completes.
    pub fn load_resource(&mut self, entry: SpaceEntryId, path: &str) {
        self.pending.insert(entry);
        self.pending_paths.insert(entry, path.to_string());
    }

    /// The completion of `addMappingAsync` (`LoadResourceCallback`): the
    /// mapping now holds its chunk.
    pub fn complete_load(&mut self, entry: SpaceEntryId) {
        let Some(path) = self.pending_paths.remove(&entry) else {
            return;
        };
        self.pending.remove(&entry);
        self.store.borrow_mut().acquire(&path);
        self.mappings.insert(
            entry,
            ChunkRef {
                store: Rc::clone(&self.store),
                path,
            },
        );
    }

    /// `PhysicalChunkSpace::unloadResource` (`physical_chunk_space.cpp:96`):
    /// remove a mapping, releasing its chunk. A pending load is cancelled.
    pub fn unload_resource(&mut self, entry: SpaceEntryId) {
        self.pending_paths.remove(&entry);
        self.pending.remove(&entry);
        self.mappings.remove(&entry);
    }

    /// `PhysicalChunkSpace::cancelCurrentlyLoading` (`:167`): drop the
    /// pending loads. Nothing was acquired yet, so nothing is released.
    pub fn cancel_currently_loading(&mut self) {
        self.pending.clear();
        self.pending_paths.clear();
    }

    /// `PhysicalChunkSpace::isFullyUnloaded` (`:163`): no mapping holds a
    /// chunk and none is loading.
    pub fn is_fully_unloaded(&self) -> bool {
        self.mappings.is_empty() && self.pending.is_empty()
    }

    /// `PhysicalChunkSpace::reuse` (`:177`): `pChunkSpace_->clear()`.
    pub fn reuse(&mut self) {
        self.clear();
    }

    /// `PhysicalChunkSpace::clear` (`:186`): unload every mapping.
    pub fn clear(&mut self) {
        self.mappings.clear();
        self.pending.clear();
        self.pending_paths.clear();
    }
}

impl Drop for PhysicalChunkSpace {
    /// The destructor unloads everything it still holds (`clear`, `:186`).
    fn drop(&mut self) {
        self.clear();
    }
}

/// `PhysicalDelegateSpace` (`physical_delegate_space.hpp`): the delegated game
/// supplies the space, so mappings are recorded but hold no chunks and the
/// space always reports fully unloaded.
#[derive(Debug, Clone, Default)]
pub struct PhysicalDelegateSpace {
    mappings: BTreeSet<SpaceEntryId>,
}

impl PhysicalDelegateSpace {
    /// A fresh delegated physical space.
    pub fn new() -> Self {
        PhysicalDelegateSpace::default()
    }

    /// `loadResource`: the delegated game owns the resource; only the mapping
    /// is recorded.
    pub fn load_resource(&mut self, entry: SpaceEntryId) {
        self.mappings.insert(entry);
    }

    /// `unloadResource`.
    pub fn unload_resource(&mut self, entry: SpaceEntryId) {
        self.mappings.remove(&entry);
    }

    /// The number of recorded mappings.
    pub fn num_mappings(&self) -> usize {
        self.mappings.len()
    }

    /// `IPhysicalSpace::clear`.
    pub fn clear(&mut self) {
        self.mappings.clear();
    }

    /// `IPhysicalSpace::isFullyUnloaded`: always true — the delegated game
    /// holds the chunks, not this app.
    pub fn is_fully_unloaded(&self) -> bool {
        true
    }

    /// `IPhysicalSpace::reuse`.
    pub fn reuse(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_a() -> crate::cellappmgr::Addr {
        (0x7f000001, 3000)
    }

    fn entry(salt: u16) -> SpaceEntryId {
        SpaceEntryId::new(app_a(), salt)
    }

    /// Mounting a mapping acquires one reference to its chunk.
    #[test]
    fn mounting_a_mapping_acquires_a_reference() {
        let mut space = PhysicalChunkSpace::new(7);
        space.load_resource(entry(1), "spaces/arena");
        assert!(space.is_mapping_pending());
        assert!(!space.is_fully_unloaded(), "a pending load is not unloaded");
        space.complete_load(entry(1));

        assert_eq!(space.num_mappings(), 1);
        assert!(!space.is_mapping_pending());
        assert!(!space.is_fully_unloaded());
        let mapping = space.mapping(entry(1)).expect("mapping completed");
        assert_eq!(mapping.path(), "spaces/arena");
        assert_eq!(mapping.refs(), 1);
    }

    /// Two spaces sharing a store hold two references to the same chunk.
    #[test]
    fn two_spaces_sharing_a_chunk_hold_two_references() {
        let store = Rc::new(RefCell::new(ChunkStore::new()));
        let mut one = PhysicalChunkSpace::shared(1, Rc::clone(&store));
        let mut two = PhysicalChunkSpace::shared(2, Rc::clone(&store));

        one.load_resource(entry(1), "spaces/arena");
        two.load_resource(entry(2), "spaces/arena");
        one.complete_load(entry(1));
        two.complete_load(entry(2));

        assert_eq!(store.borrow().ref_count("spaces/arena"), 2);
        assert_eq!(store.borrow().num_chunks(), 1, "one chunk, two holders");
    }

    /// The acceptance: destroying one space releases only its reference; the
    /// chunk survives while the other space still holds it, and is freed when
    /// the last holder goes.
    #[test]
    fn references_are_released_after_destruction() {
        let store = Rc::new(RefCell::new(ChunkStore::new()));
        let mut one = PhysicalChunkSpace::shared(1, Rc::clone(&store));
        let mut two = PhysicalChunkSpace::shared(2, Rc::clone(&store));

        one.load_resource(entry(1), "spaces/arena");
        two.load_resource(entry(2), "spaces/arena");
        one.complete_load(entry(1));
        two.complete_load(entry(2));

        drop(one);
        assert_eq!(store.borrow().ref_count("spaces/arena"), 1);
        assert!(store.borrow().is_live("spaces/arena"));

        drop(two);
        assert!(!store.borrow().is_live("spaces/arena"), "chunk is freed");
        assert_eq!(store.borrow().num_chunks(), 0);
    }

    /// Unmounting a mapping releases its chunk at once when it was the last
    /// reference (`unloadResource`).
    #[test]
    fn unmounting_the_last_mapping_frees_the_chunk() {
        let mut space = PhysicalChunkSpace::new(7);
        space.load_resource(entry(1), "spaces/arena");
        space.complete_load(entry(1));

        space.unload_resource(entry(1));
        assert_eq!(space.num_mappings(), 0);
        assert!(space.is_fully_unloaded());
        assert!(space.mapping(entry(1)).is_none());
    }

    /// Cancelling a pending load releases nothing, because nothing was
    /// acquired (`cancelCurrentlyLoading`).
    #[test]
    fn cancelling_a_pending_load_releases_nothing() {
        let mut space = PhysicalChunkSpace::new(7);
        space.load_resource(entry(1), "spaces/arena");
        space.cancel_currently_loading();

        assert!(!space.is_mapping_pending());
        assert_eq!(space.num_mappings(), 0);
        assert!(space.is_fully_unloaded());
        // Completing a cancelled load is a no-op.
        space.complete_load(entry(1));
        assert_eq!(space.num_mappings(), 0);
    }

    /// Clear and reuse unload every mapping (`clear`, `reuse`).
    #[test]
    fn clear_and_reuse_unload_everything() {
        let mut space = PhysicalChunkSpace::new(7);
        space.load_resource(entry(1), "a");
        space.load_resource(entry(2), "b");
        space.complete_load(entry(1));
        space.complete_load(entry(2));

        space.reuse();
        assert_eq!(space.num_mappings(), 0);
        assert!(space.is_fully_unloaded());

        space.load_resource(entry(3), "c");
        space.clear();
        assert_eq!(space.num_mappings(), 0);
        assert!(space.is_fully_unloaded());
    }

    /// Dropping the space unloads what it held — the reference goes with the
    /// space, even without an explicit clear.
    #[test]
    fn dropping_the_space_releases_its_chunks() {
        let store = Rc::new(RefCell::new(ChunkStore::new()));
        let mut space = PhysicalChunkSpace::shared(7, Rc::clone(&store));
        space.load_resource(entry(1), "a");
        space.complete_load(entry(1));
        space.load_resource(entry(2), "b");
        space.complete_load(entry(2));

        drop(space);
        assert_eq!(store.borrow().num_chunks(), 0);
    }

    /// Each mapping of the same path holds its own reference, so unloading
    /// one of them keeps the chunk alive.
    #[test]
    fn two_mappings_of_one_path_hold_two_references() {
        let mut space = PhysicalChunkSpace::new(7);
        space.load_resource(entry(1), "shared");
        space.load_resource(entry(2), "shared");
        space.complete_load(entry(1));
        space.complete_load(entry(2));

        assert_eq!(space.mapping(entry(1)).unwrap().refs(), 2);
        space.unload_resource(entry(1));
        assert_eq!(space.mapping(entry(2)).unwrap().refs(), 1);
        assert!(!space.is_fully_unloaded());
    }

    /// The delegated physical space records mappings, holds no chunks and is
    /// always fully unloaded.
    #[test]
    fn a_delegated_space_is_always_fully_unloaded() {
        let mut space = PhysicalDelegateSpace::new();
        assert!(space.is_fully_unloaded());

        space.load_resource(entry(1));
        space.load_resource(entry(2));
        assert_eq!(space.num_mappings(), 2);
        assert!(space.is_fully_unloaded(), "delegated holds no chunks");

        space.unload_resource(entry(1));
        assert_eq!(space.num_mappings(), 1);
        space.reuse();
        assert_eq!(space.num_mappings(), 0);
    }

    /// A completed load of an unknown mapping id is ignored, as the reference
    /// callback only fires for the ids it was given.
    #[test]
    fn completing_an_unknown_load_is_ignored() {
        let mut space = PhysicalChunkSpace::new(7);
        space.complete_load(entry(9));
        assert_eq!(space.num_mappings(), 0);
        assert!(space.is_fully_unloaded());
    }

    /// The store reports zero for a chunk it never held, and releasing an
    /// unknown chunk frees nothing.
    #[test]
    fn the_store_ignores_unknown_chunks() {
        let mut store = ChunkStore::new();
        assert_eq!(store.ref_count("nowhere"), 0);
        assert!(!store.is_live("nowhere"));
        assert!(!store.release("nowhere"), "nothing was freed");
        assert_eq!(store.num_chunks(), 0);
    }
}
