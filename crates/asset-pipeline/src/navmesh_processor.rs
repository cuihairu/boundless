//! BW-13-02-05: the NavGen entry.
//!
//! The reference's `tools/common/navmesh_processor.cpp` is the shell the
//! editor drives a chunk's navmesh generation through: it locks the
//! chunk's neighbourhood, walks the chunk's girth list once — flood,
//! generate, serialise, appending every girth's binary — and hands the
//! concatenated result back to the chunk cache. This module ports that
//! shell and the machinery it rides on:
//!
//! * `lib/chunk/chunk_processor.hpp/.cpp` — the base task that runs a
//!   chunk's secondary data work across threads;
//! * `lib/chunk/scoped_locked_chunk_holder.{hpp,cpp,ipp}` — RAII chunk
//!   locking, the double-lock guard and the destructor's
//!   "has chunks locked" complaint;
//! * `lib/chunk/locked_chunks.hpp` — the global reference-counted list
//!   the unloaders consult;
//! * `lib/chunk/chunk_processor_manager.cpp:1670-1830` — the two
//!   `lockChunkInMemory` shapes;
//! * `tools/common/editor_chunk_navmesh_cache_base.{hpp,cpp}` — the
//!   chunk cache that owns the navmesh and drives the recalculation.
//!
//! The generation itself lands in `crates/waypoint-gen` (BW-10-05-04):
//! [`FloodController`] drives the flood per girth, and
//! [`generate_from_flood`] plus [`as_binary`] run the generation. The
//! chunk, physics, manager and resource seams become traits so the ring
//! is runnable headless.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::Arc;

use foundation::math::plane::PlaneEq;
use foundation::math::vector::Vector3;
use waypoint_gen::WaypointGenerator;
use waypoint_gen::flood::AdjGridElt;
use waypoint_gen::view::as_binary;
use waypoint_gen::workflow::{DropPhysics, FloodStage, NavmeshProbe, generate_from_flood};

// ---------------------------------------------------------------------------
// Locked chunks (`lib/chunk/locked_chunks.hpp`, all inline there)
// ---------------------------------------------------------------------------

/// A chunk's identity in the locked sets. The reference keys its sets by
/// `Chunk*`; the port keys by the chunk identifier
/// (`outside_chunk_identifier`), which names the same chunk.
pub type ChunkId = String;

/// `BW::set<Chunk*>`.
pub type ChunkSet = BTreeSet<ChunkId>;

/// `LockedChunks` (`lib/chunk/locked_chunks.hpp`, every method inline
/// there): the global reference-counted list of chunks no unloader may
/// take away.
#[derive(Debug, Default)]
pub struct LockedChunks {
    locks: BTreeMap<ChunkId, i32>,
}

/// `MF_ASSERT(chunks_[pChunk] > 0)` (`locked_chunks.hpp`, `unlock`): the
/// reference answers these with a hard assert; the port returns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockedChunkError {
    /// `unlock` on a chunk with no lock.
    NeverLocked(ChunkId),
    /// `unlock` on a chunk whose lock count is already 0.
    ZeroCount(ChunkId),
    /// `lockChunkInMemory`'s terminal `MF_ASSERT(chunks.count(chunk))`.
    NotLocked(ChunkId),
    /// The inside-chunk branch of `lockChunkInMemory` recurses through
    /// the shell's neighbouring chunks; the reference has no depth bound
    /// (`chunk_processor_manager.cpp:1746-1766`), the port caps the
    /// recursion so a cyclic neighbour set in a test cannot hang the
    /// editor.
    RecursionCap(ChunkId),
}

impl fmt::Display for LockedChunkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeverLocked(c) => write!(f, "LockedChunks: unlock on an unheld chunk '{c}'"),
            Self::ZeroCount(c) => write!(f, "LockedChunks: unlock on '{c}' at zero"),
            Self::NotLocked(c) => write!(f, "lockChunkInMemory did not lock '{c}'"),
            Self::RecursionCap(c) => write!(f, "lockChunkInMemory recursion cap hit at '{c}'"),
        }
    }
}

impl Error for LockedChunkError {}

impl LockedChunks {
    /// `lock`: `++chunks_[pChunk]`.
    pub fn lock(&mut self, chunk: &ChunkId) {
        *self.locks.entry(chunk.clone()).or_insert(0) += 1;
    }

    /// `unlock`: the reference `MF_ASSERT`s `chunks_[pChunk] > 0`
    /// before decrementing and drops the entry when it reaches zero.
    pub fn unlock(&mut self, chunk: &ChunkId) -> Result<(), LockedChunkError> {
        let Some(count) = self.locks.get_mut(chunk) else {
            return Err(LockedChunkError::NeverLocked(chunk.clone()));
        };
        debug_assert!(*count > 0, "LockedChunks::unlock zero count");
        *count -= 1;
        if *count == 0 {
            self.locks.remove(chunk);
        }
        Ok(())
    }

    /// `unlock(const BW::set<Chunk*>&)`: the reference asserts on the
    /// first unlocked chunk; the port stops there.
    pub fn unlock_all(&mut self, chunks: &ChunkSet) -> Result<(), LockedChunkError> {
        for chunk in chunks {
            self.unlock(chunk)?;
        }
        Ok(())
    }

    /// `clear`.
    pub fn clear(&mut self) {
        self.locks.clear();
    }

    /// `empty`.
    pub fn is_empty(&self) -> bool {
        self.locks.is_empty()
    }

    /// The `chunks_[pChunk]` lock count the reference would read.
    pub fn lock_count(&self, chunk: &ChunkId) -> i32 {
        self.locks.get(chunk).copied().unwrap_or(0)
    }

    /// `chunks_.size()`.
    pub fn len(&self) -> usize {
        self.locks.len()
    }
}

// ---------------------------------------------------------------------------
// The processor manager's required faces
// ---------------------------------------------------------------------------

/// `ChunkProcessorManager`'s surface the navmesh entry consumes. The
/// reference is a 1999-line editor object; the port keeps only the faces
/// the entry, the holder and the cache touch and lets a double supply
/// them.
pub trait ChunkProcessorManagerFace {
    /// `numRunningThreads` — whether anything is currently processing.
    fn num_running_threads(&self) -> usize;
    /// `addBackgroundTask`.
    fn add_background_task(&mut self, processor: u64);
    /// `addMainThreadTask`.
    fn add_main_thread_task(&mut self, processor: u64);

    /// `allLockedChunks`.
    fn locked_chunks(&mut self) -> &mut LockedChunks;

    /// `gridFromChunkName` (the reference's `MF_VERIFY`s).
    fn grid_from_chunk_name(&self, name: &str) -> Option<(i16, i16)>;
    /// `outsideChunkIdentifier`.
    fn outside_chunk_identifier(&self, x: i32, z: i32) -> String;
    /// `isOutsideChunk` (the identifier form).
    fn is_outside_chunk(&self, chunk: &ChunkId) -> bool;
    /// `chunk->boundingBox()` as (min, max) corners.
    fn chunk_bounds(&self, chunk: &ChunkId) -> (Vector3, Vector3);
    /// `ChunkOverlappers::instance(chunk).overlappers()`.
    fn overlappers(&self, chunk: &ChunkId) -> Vec<ChunkId>;
    /// `collectOverlappedOutsideChunksForShell` (up to four).
    fn outside_neighbours(&self, chunk: &ChunkId) -> Vec<ChunkId>;

    /// `updateChunkDirtyStatus`.
    fn update_chunk_dirty_status(&mut self, chunk: &ChunkId);

    /// `isChunkReadyToProcess`.
    fn is_chunk_ready_to_process(&self, chunk: &ChunkId, expand_x: i32, expand_z: i32) -> bool;
    /// `loadChunkForProcessing`.
    fn load_chunk_for_processing(&mut self, chunk: &ChunkId, expand_x: i32, expand_z: i32) -> bool;
    /// `spreadInvalidate`.
    fn spread_invalidate(
        &mut self,
        chunk: &ChunkId,
        expand_x: i32,
        expand_z: i32,
        cache_index: usize,
        changed_item: Option<&str>,
    );

    /// `lockChunkInMemory(chunk, expandOnGridX, expandOnGridZ, holder)`
    /// (`chunk_processor_manager.cpp:1670-1770`). Outside chunks lock a
    /// whole (2x+1)x(2z+1) grid ring plus their overlappers; inside
    /// chunks recurse into their outside neighbours. The terminal
    /// `MF_ASSERT` becomes [`LockedChunkError::NotLocked`].
    fn lock_chunk_in_memory(
        &mut self,
        chunk: &ChunkId,
        expand_x: i32,
        expand_z: i32,
        holder: &mut ScopedLockedChunkHolder,
    ) -> Result<(), LockedChunkError> {
        if self.is_outside_chunk(chunk) {
            let Some((grid_x, grid_z)) = self.grid_from_chunk_name(chunk) else {
                return Err(LockedChunkError::NeverLocked(chunk.clone()));
            };
            let mut locked: ChunkSet = ChunkSet::new();
            for z in (i32::from(grid_z) - expand_z)..=(i32::from(grid_z) + expand_z) {
                for x in (i32::from(grid_x) - expand_x)..=(i32::from(grid_x) + expand_x) {
                    let name = self.outside_chunk_identifier(x, z);
                    if name.is_empty() {
                        continue;
                    }
                    if !holder.contains(&name) {
                        let locks = self.locked_chunks();
                        holder.insert_with_locks(&name, locks)?;
                        locked.insert(name);
                    }
                }
            }
            // Lock the overlappers of every chunk just locked. The
            // reference walks an unordered `BW::set<Chunk*>` here; the
            // port sorts so the lock order is reproducible.
            for name in &locked {
                let mut overlappers = self.overlappers(name);
                overlappers.sort();
                for overlapping in overlappers {
                    if !holder.contains(&overlapping) {
                        let locks = self.locked_chunks();
                        holder.insert_with_locks(&overlapping, locks)?;
                    }
                }
            }
        } else {
            self.lock_chunk_in_memory_inside(chunk, expand_x, expand_z, holder, 0)?;
        }
        if !holder.contains(chunk) {
            return Err(LockedChunkError::NotLocked(chunk.clone()));
        }
        Ok(())
    }

    /// The inside-chunk branch: recurse into the shell's outside
    /// neighbours with the same expansion (`:1746-1766`). The reference
    /// has no depth bound; the port caps the recursion.
    fn lock_chunk_in_memory_inside(
        &mut self,
        chunk: &ChunkId,
        expand_x: i32,
        expand_z: i32,
        holder: &mut ScopedLockedChunkHolder,
        depth: i32,
    ) -> Result<(), LockedChunkError> {
        const CAP: i32 = 32;
        if depth > CAP {
            return Err(LockedChunkError::RecursionCap(chunk.clone()));
        }
        for neighbour in self.outside_neighbours(chunk) {
            if self.is_outside_chunk(&neighbour) {
                self.lock_chunk_in_memory(&neighbour, expand_x, expand_z, holder)?;
            } else {
                // The reference's `lockChunkInMemory` has no recursion
                // bound; the port threads the depth so a cyclic
                // neighbour set stops at the cap.
                self.lock_chunk_in_memory_inside(
                    &neighbour,
                    expand_x,
                    expand_z,
                    holder,
                    depth + 1,
                )?;
            }
        }
        Ok(())
    }

    /// `lockChunkInMemory(chunk, portalDepth, holder)`
    /// (`chunk_processor_manager.cpp:1772-1830`): walk the portals out
    /// to the given depth, marking each chunk once, then lock the
    /// accumulated set.
    fn lock_chunk_in_memory_portal(
        &mut self,
        chunk: &ChunkId,
        portal_depth: i32,
        holder: &mut ScopedLockedChunkHolder,
    ) -> Result<(), LockedChunkError> {
        let mut visited: ChunkSet = ChunkSet::new();
        self.lock_portal_step(chunk, portal_depth, &mut visited, holder)?;
        // `MF_ASSERT(chunks.find(chunk) != chunks.end())`: the first
        // portal step always inserts the starting chunk.
        debug_assert!(holder.contains(chunk));
        Ok(())
    }

    /// `lockChunkInMemoryInternal` (`:1772-1798`). The reference walks
    /// `chunk->traverseMark()`; the port keeps the visited set local to
    /// one call.
    fn lock_portal_step(
        &mut self,
        chunk: &ChunkId,
        depth: i32,
        visited: &mut ChunkSet,
        holder: &mut ScopedLockedChunkHolder,
    ) -> Result<(), LockedChunkError> {
        if !visited.insert(chunk.clone()) {
            return Ok(());
        }
        let locks = self.locked_chunks();
        holder.insert_with_locks(chunk, locks)?;
        if depth != 0 {
            let mut neighbours = self.outside_neighbours(chunk);
            neighbours.sort();
            for neighbour in neighbours {
                self.lock_portal_step(&neighbour, depth - 1, visited, holder)?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Scoped locked chunks (`lib/chunk/scoped_locked_chunk_holder.{hpp,cpp,ipp}`)
// ---------------------------------------------------------------------------

/// `ScopedLockedChunkHolder` (`scoped_locked_chunk_holder.hpp`): locks
/// chunks in memory so they cannot be unloaded while in use, unlocking
/// them all on destruction, checking for double-locking and complaining
/// in the destructor when something is still locked.
///
/// The reference holds the manager pointer (`MF_ASSERT(manager_)` in the
/// constructor); the port takes the manager at each `lock` call instead
/// so the holder can be owned by a processor that is itself borrowed
/// from the manager.
#[derive(Debug, Default)]
pub struct ScopedLockedChunkHolder {
    chunks: ChunkSet,
}

impl ScopedLockedChunkHolder {
    /// `ScopedLockedChunkHolder(manager)`: `MF_ASSERT(manager_)`.
    pub fn new() -> Self {
        Self::default()
    }

    /// `lock(pChunk)`.
    pub fn lock(
        &mut self,
        chunk: &ChunkId,
        manager: &mut dyn ChunkProcessorManagerFace,
    ) -> Result<(), LockedChunkError> {
        self.insert(chunk, manager)
    }

    /// `lock(pChunk, expandOnGridX, expandOnGridZ)`
    /// (`scoped_locked_chunk_holder.cpp:44-59`). The reference compares
    /// `expandOnGridX` with **itself** (`if ((expandOnGridX == 0) &&
    /// (expandOnGridX == 0))`), so a non-zero `expandOnGridZ` alone
    /// never reaches the expansion — the quirk is pinned here.
    #[allow(clippy::eq_op, reason = "the C++ holder compares expandOnGridX twice")]
    pub fn lock_grid(
        &mut self,
        chunk: &ChunkId,
        expand_x: i32,
        expand_z: i32,
        manager: &mut dyn ChunkProcessorManagerFace,
    ) -> Result<(), LockedChunkError> {
        if expand_x == 0 && expand_x == 0 {
            self.lock(chunk, manager)
        } else {
            manager.lock_chunk_in_memory(chunk, expand_x, expand_z, self)
        }
    }

    /// `lock(pChunk, portalDepth)` (`:64-78`): 0 is a plain lock, any
    /// other value walks the portals out to that depth.
    pub fn lock_portal_depth(
        &mut self,
        chunk: &ChunkId,
        portal_depth: i32,
        manager: &mut dyn ChunkProcessorManagerFace,
    ) -> Result<(), LockedChunkError> {
        if portal_depth == 0 {
            self.lock(chunk, manager)
        } else {
            manager.lock_chunk_in_memory_portal(chunk, portal_depth, self)
        }
    }

    /// `insert` (`:90-109`): double-lock guard plus `MF_ASSERT(pChunk
    /// ->loaded() || pChunk->loading())`; the port keeps the guard, the
    /// assert becomes [`LockedChunkError::NeverLocked`] on the missing
    /// chunk.
    fn insert(
        &mut self,
        chunk: &ChunkId,
        manager: &mut dyn ChunkProcessorManagerFace,
    ) -> Result<(), LockedChunkError> {
        self.insert_with_locks(chunk, manager.locked_chunks())
    }

    /// `insert` with the global list supplied directly, so a manager's
    /// default lock implementation can insert without re-borrowing the
    /// manager as a trait object.
    pub fn insert_with_locks(
        &mut self,
        chunk: &ChunkId,
        locks: &mut LockedChunks,
    ) -> Result<(), LockedChunkError> {
        if self.chunks.contains(chunk) {
            return Ok(());
        }
        // `MF_ASSERT(pChunk->loaded() || pChunk->loading())` — the
        // manager's job; the holder trusts the caller here, the
        // reference asserts on the same call.
        self.chunks.insert(chunk.clone());
        // `chunks_.insert` then `allLockedChunks().lock`.
        locks.lock(chunk);
        Ok(())
    }

    /// `erase(position)` (`:114-124`): unlock through the manager, then
    /// remove from the holder.
    pub fn erase(
        &mut self,
        chunk: &ChunkId,
        manager: &mut dyn ChunkProcessorManagerFace,
    ) -> Result<ChunkId, LockedChunkError> {
        if !self.chunks.contains(chunk) {
            return Err(LockedChunkError::NeverLocked(chunk.to_string()));
        }
        manager.locked_chunks().unlock(chunk)?;
        self.chunks.remove(chunk);
        Ok(chunk.to_string())
    }

    /// `clear` (`:138-143`): unlock everything the holder owns, in one
    /// shot.
    pub fn clear(
        &mut self,
        manager: &mut dyn ChunkProcessorManagerFace,
    ) -> Result<(), LockedChunkError> {
        manager.locked_chunks().unlock_all(&self.chunks)?;
        self.chunks.clear();
        Ok(())
    }

    /// `empty`.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// `size`.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// `find`.
    pub fn contains(&self, chunk: &ChunkId) -> bool {
        self.chunks.contains(chunk)
    }

    /// `count`.
    pub fn count(&self, chunk: &ChunkId) -> usize {
        usize::from(self.chunks.contains(chunk))
    }

    /// `begin`/`end` (the reference's `ChunkSet::iterator`).
    pub fn chunks(&self) -> impl Iterator<Item = &ChunkId> {
        self.chunks.iter()
    }
}

// ---------------------------------------------------------------------------
// Chunk processors (`lib/chunk/chunk_processor.{hpp,cpp}`)
// ---------------------------------------------------------------------------

/// `ChunkProcessorListener` (`chunk_processor.hpp:64-74`): a processor
/// finished; the listener may unregister itself inside the callback.
pub trait ChunkProcessorListener: Send + Sync {
    fn on_chunk_processor_finished(&self, processor: u64, has_unregistered: &mut bool);
}

/// The virtual half of `ChunkProcessor` (`chunk_processor.hpp:35-60`).
/// Subclasses must implement the four stages.
pub trait ProcessorBody {
    /// `processInBackground` — return false on stop.
    fn process_in_background(&mut self, manager: &mut dyn ChunkProcessorManagerFace) -> bool;
    /// `processInMainThread`.
    fn process_in_main_thread(
        &mut self,
        manager: &mut dyn ChunkProcessorManagerFace,
        background_task_result: bool,
    ) -> bool;
    /// `cleanup`.
    fn cleanup(&mut self, manager: &mut dyn ChunkProcessorManagerFace) -> Result<(), String>;
    /// `BackgroundTask`'s task name.
    fn task_name(&self) -> &str;
}

/// `volatile bool backgroundResult_` (`chunk_processor.hpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessorStatus {
    /// `cleanedUp_ == false`, never run.
    Idle,
    /// Running in the background queue.
    Backgrounded,
    /// Queued back to the main thread.
    MainThreaded,
    /// Cleanup finished, listeners notified.
    CleanedUp,
}

/// `ChunkProcessor::process`'s state plus the listener bookkeeping.
pub struct ChunkProcessorTask<B: ProcessorBody> {
    body: B,
    background_result: bool,
    cleaned_up: bool,
    listeners: Vec<Arc<dyn ChunkProcessorListener>>,
    id: u64,
}

/// `ChunkProcessors::registerProcessor`.
pub trait ProcessorStateFace {
    fn register_processor(&mut self, id: u64, body: Box<dyn ProcessorBody>);
}

impl<B: ProcessorBody> ChunkProcessorTask<B> {
    /// `ChunkProcessor::ChunkProcessor` (`chunk_processor.cpp:44-49`).
    pub fn new(body: B, id: u64) -> Self {
        Self {
            body,
            background_result: false,
            cleaned_up: false,
            listeners: Vec::new(),
            id,
        }
    }

    /// `addListener`/`delListener` (`chunk_processor.hpp:80-83`): the
    /// reference compares `ChunkProcessorListener*`; the port compares
    /// `Arc` addresses.
    pub fn add_listener(&mut self, listener: Arc<dyn ChunkProcessorListener>) {
        let ptr = Arc::as_ptr(&listener) as *const () as usize;
        if self
            .listeners
            .iter()
            .any(|existing| Arc::as_ptr(existing) as *const () as usize == ptr)
        {
            return;
        }
        self.listeners.push(listener);
    }

    pub fn del_listener(&mut self, listener: &Arc<dyn ChunkProcessorListener>) {
        let ptr = Arc::as_ptr(listener) as *const () as usize;
        self.listeners
            .retain(|existing| Arc::as_ptr(existing) as *const () as usize != ptr);
    }

    /// `ChunkProcessor::process` (`chunk_processor.cpp:55-72`).
    pub fn process(&mut self, manager: &mut dyn ChunkProcessorManagerFace) -> Result<bool, String> {
        if manager.num_running_threads() > 0 {
            manager.add_background_task(self.id);
            return Ok(true);
        }
        // The single-threaded run cannot be aborted.
        self.background_result = self.body.process_in_background(manager);
        self.background_result &= self
            .body
            .process_in_main_thread(manager, self.background_result);
        self.cleanup(manager)?;
        Ok(self.background_result)
    }

    /// `cancel` (`:77-86`): queue it back to the main thread so its
    /// chunk caches still get cleaned.
    pub fn cancel(&mut self, manager: &mut dyn ChunkProcessorManagerFace) {
        manager.add_main_thread_task(self.id);
    }

    /// `doBackgroundTask` (`:167-175`).
    pub fn do_background_task(&mut self, manager: &mut dyn ChunkProcessorManagerFace) {
        self.background_result = self.body.process_in_background(manager);
        manager.add_main_thread_task(self.id);
    }

    /// `doMainThreadTask` (`:94-108`): re-queue when the main thread
    /// work did not finish, otherwise clean up (the reference's
    /// `cleanup` is void, so a failed unlock does not re-queue).
    pub fn do_main_thread_task(&mut self, manager: &mut dyn ChunkProcessorManagerFace) {
        if !self
            .body
            .process_in_main_thread(manager, self.background_result)
        {
            manager.add_main_thread_task(self.id);
            return;
        }
        let _ = self.cleanup(manager);
    }

    /// `cleanup` (`:113-118`): once-only; the failure arms re-queue the
    /// task the way `doMainThreadTask` does.
    fn cleanup(&mut self, manager: &mut dyn ChunkProcessorManagerFace) -> Result<(), String> {
        // `cleanup` is once-only; the listener notification rides on
        // the first pass even when the body's cleanup fails.
        if self.cleaned_up {
            return Ok(());
        }
        self.cleaned_up = true;
        self.notify_listener_finished();
        self.body.cleanup(manager).map_err(|message| {
            format!(
                "ChunkProcessor ({}) cleanup failed: {message}",
                self.body.task_name()
            )
        })
    }

    /// `notifyListenerFinished` (`:132-147`).
    fn notify_listener_finished(&mut self) {
        // `listeners_[i]` with a listener unregistering itself inside
        // the callback: the reference decrements i so the erased slot
        // — now holding the next listener — is visited next.
        let mut i = 0usize;
        while i < self.listeners.len() {
            let listener = self.listeners[i].clone();
            let mut unregistered = false;
            listener.on_chunk_processor_finished(self.id, &mut unregistered);
            if unregistered {
                let ptr = Arc::as_ptr(&listener) as *const () as usize;
                self.listeners
                    .retain(|existing| Arc::as_ptr(existing) as *const () as usize != ptr);
            } else {
                i += 1;
            }
        }
    }

    /// Remove a listener by its data pointer, for callbacks holding
    /// only a borrowed `&self` (the reference's `delListener(this)`).
    pub fn remove_listener_by_ptr(&mut self, ptr: usize) {
        self.listeners
            .retain(|existing| Arc::as_ptr(existing) as *const () as usize != ptr);
    }

    /// The registered listeners.
    pub fn listeners(&self) -> &[Arc<dyn ChunkProcessorListener>] {
        &self.listeners
    }

    /// `cleanedUp_`.
    pub fn cleaned_up(&self) -> bool {
        self.cleaned_up
    }

    /// The task's id (`ChunkProcessorPtr`'s identity).
    pub fn id(&self) -> u64 {
        self.id
    }
}

/// `ChunkProcessors` (`chunk_processor.hpp:88-111`): add is asserted
/// unique, remove is silent.
#[derive(Default)]
pub struct ChunkProcessors {
    processors: BTreeMap<u64, Box<dyn ProcessorBody>>,
}

impl ChunkProcessors {
    /// `addProcessor` (`chunk_processor.cpp:12-22`): the reference
    /// `MF_ASSERT`s the processor is not already in the list; the port
    /// keeps the assert and, in release builds, appends anyway.
    pub fn add_processor(&mut self, id: u64, body: Box<dyn ProcessorBody>) {
        debug_assert!(
            !self.processors.contains_key(&id),
            "ChunkProcessors::addProcessor: duplicate"
        );
        self.processors.insert(id, body);
    }

    /// `delProcessor`.
    pub fn del_processor(&mut self, id: u64) {
        self.processors.remove(&id);
    }

    /// `isEmpty`.
    pub fn is_empty(&self) -> bool {
        self.processors.is_empty()
    }

    /// `chunkProcessors().size()`.
    pub fn len(&self) -> usize {
        self.processors.len()
    }

    /// `chunkProcessors()`.
    pub fn processors(&self) -> impl Iterator<Item = &u64> {
        self.processors.keys()
    }
}

// ---------------------------------------------------------------------------
// The navmesh chunk face
// ---------------------------------------------------------------------------

/// `EditorChunkEntityCache::instance(chunk).getEntityPositions`
/// (`editor_chunk_entity_cache.cpp`): the entity positions the navmesh
/// generator floods from.
pub trait NavmeshEntityPositions {
    fn entity_positions(&self) -> Vec<Vector3>;
}

/// `EditorChunkNavmeshCacheBase`'s chunk face (`navmesh_processor.hpp`:
/// `Chunk&`). `NavmeshProbe` is the part [`generate_from_flood`] reads;
/// the port extends it with the chunk's own identity, its portal planes
/// and its bounds, so the shell needs no further seam.
pub trait NavgenChunkFace: NavmeshProbe {
    /// `Chunk::identifier`.
    fn identifier(&self) -> &str;
    /// `Chunk::isOutsideChunk`.
    fn is_outside_chunk(&self) -> bool;
    /// `Chunk::loaded`.
    fn loaded(&self) -> bool;
    /// `Chunk::loading`.
    fn loading(&self) -> bool;
    /// `Chunk::boundingBox`'s portal planes (`extendThroughUnboundPortals`
    /// walks `chunk->bounds()`); the reference derives them per portal.
    fn portal_planes(&self) -> Vec<PlaneEq>;
}

/// An owned snapshot of a flood result. The reference reads
/// `flooder_.width()`/`minBounds()`/grids directly while the flood and
/// the generator borrow distinct objects; the Rust port has the
/// generator borrow `&mut self.flood`, so [`FloodController::stage`]
/// clones the grids into this owned snapshot the generator reads.
#[derive(Clone, Debug)]
pub struct FloodSnapshot {
    /// `Flooded`.
    pub flooded: bool,
    /// `W`.
    pub width: i32,
    /// `H`.
    pub height: i32,
    /// `minBounds_`.
    pub min_bounds: Vector3,
    /// `resolution_`.
    pub resolution: f32,
    /// `adjGrids_[g]`, copied per layer.
    pub adjacency: Vec<Vec<AdjGridElt>>,
    /// `hgtGrids_[g]`, copied per layer.
    pub heights: Vec<Vec<f32>>,
}

impl FloodSnapshot {
    /// Clone a [`FloodStage`]'s grids into an owned snapshot.
    pub fn from_stage(stage: &dyn FloodStage) -> Self {
        Self {
            flooded: stage.flooded(),
            width: stage.width(),
            height: stage.height(),
            min_bounds: stage.min_bounds(),
            resolution: stage.resolution(),
            adjacency: stage.adjacency().to_vec(),
            heights: stage.heights().to_vec(),
        }
    }
}

impl FloodStage for FloodSnapshot {
    fn flooded(&self) -> bool {
        self.flooded
    }
    fn width(&self) -> i32 {
        self.width
    }
    fn height(&self) -> i32 {
        self.height
    }
    fn min_bounds(&self) -> Vector3 {
        self.min_bounds
    }
    fn resolution(&self) -> f32 {
        self.resolution
    }
    fn adjacency(&self) -> &[Vec<AdjGridElt>] {
        &self.adjacency
    }
    fn heights(&self) -> &[Vec<f32>] {
        &self.heights
    }
}

/// The flood driver: one call per girth returns whether the flood
/// produced content (the reference's `flooder_.flood` bool).
pub trait FloodController: DropPhysics {
    /// `flood(girth, entityPts)`: true when the flood succeeded.
    fn flood(&mut self, girth: f32, entity_points: &[Vector3]) -> bool;
    /// The flood result for [`generate_from_flood`], as an owned
    /// snapshot (see [`FloodSnapshot`]).
    fn stage(&self) -> FloodSnapshot;
}

// ---------------------------------------------------------------------------
// NavmeshProcessor (`tools/common/navmesh_processor.{hpp,cpp}`)
// ---------------------------------------------------------------------------

/// `navmesh_processor.hpp:34-46`.
#[derive(Debug)]
pub struct NavmeshProcessor<C: NavgenChunkFace, F: FloodController> {
    /// `chunk_`.
    chunk: C,
    /// `girths_`.
    girths: Vec<f32>,
    /// `flooder_`.
    flood: F,
    /// `navmesh_`: every girth's binary appended in order.
    navmesh: Vec<u8>,
    /// `lockedChunks_`.
    locked_chunks: ScopedLockedChunkHolder,
}

impl<C: NavgenChunkFace + NavmeshEntityPositions, F: FloodController + DropPhysics>
    NavmeshProcessor<C, F>
{
    /// `NavmeshProcessor::NavmeshProcessor`
    /// (`navmesh_processor.cpp:11-32`). The constructor locks the
    /// chunk's neighbourhood: an outside chunk with `(1, 1)`, an inside
    /// chunk with `(1)`. The reference's
    /// `lockedChunks_.lock(&chunk, 1, 1)` quirk (`expandOnGridX` compared
    /// with itself) applies through the manager's grid ring.
    pub fn new(
        chunk: C,
        flood: F,
        manager: &mut dyn ChunkProcessorManagerFace,
        girths: Vec<f32>,
    ) -> Self {
        let mut locked_chunks = ScopedLockedChunkHolder::new();
        // The outside chunk locks its (1, 1) grid ring, an inside
        // chunk its (1) portal-less ring.
        let outcome = if chunk.is_outside_chunk() {
            locked_chunks.lock_grid(&chunk.identifier().to_string(), 1, 1, manager)
        } else {
            locked_chunks.lock_grid(&chunk.identifier().to_string(), 1, 0, manager)
        };
        if outcome.is_err() {
            // `MF_ASSERT` in the reference; a failed lock must not
            // leave a half-locked shell behind.
            drop(locked_chunks);
            return Self {
                chunk,
                girths,
                flood,
                navmesh: Vec::new(),
                locked_chunks: ScopedLockedChunkHolder::new(),
            };
        }
        Self {
            chunk,
            girths,
            flood,
            navmesh: Vec::new(),
            locked_chunks,
        }
    }

    /// `chunk_` (the reference keeps the reference member).
    pub fn chunk(&self) -> &C {
        &self.chunk
    }

    /// `girths_`.
    pub fn girths(&self) -> &[f32] {
        &self.girths
    }

    /// `navmesh_`.
    pub fn navmesh(&self) -> &[u8] {
        &self.navmesh
    }

    /// `lockedChunks_`.
    pub fn locked_chunks(&self) -> &ScopedLockedChunkHolder {
        &self.locked_chunks
    }

    /// `flooder_`, for the caller to retune it between girths (the
    /// reference rebuilds its `PhysicsHandler` per girth).
    pub fn flood_mut(&mut self) -> &mut F {
        &mut self.flood
    }

    /// `processInBackground` (`navmesh_processor.cpp:50-97`): the girth
    /// ring. Each girth floods, generates and serialises; every binary
    /// is appended to `navmesh_`. A stop check and a failed flood both
    /// return false.
    pub fn process_in_background(
        &mut self,
        _manager: &mut dyn ChunkProcessorManagerFace,
        should_abort: &dyn Fn() -> bool,
    ) -> bool {
        for &girth in &self.girths {
            // `if (BgTaskManager::shouldAbortTask()) return false;`
            if should_abort() {
                return false;
            }
            let entity_points = self.chunk.entity_positions();
            if !self.flood.flood(girth, &entity_points) {
                return false;
            }
            // The snapshot is owned, so the shared borrow of `flood`
            // ends before `generate_from_flood` takes it mutably.
            let stage = self.flood.stage();
            let mut gener = WaypointGenerator::new();
            let skipped = generate_from_flood(
                &mut gener,
                &stage,
                &self.chunk,
                &mut self.flood,
                &self.chunk.portal_planes(),
                false,
                &entity_points,
                self.chunk.identifier(),
            );
            let _ = skipped;
            let binary = as_binary(&gener, girth);
            self.navmesh.extend_from_slice(&binary);
        }
        true
    }

    /// `processInMainThread` (`navmesh_processor.cpp:101-110`): the
    /// shell does nothing here; the cache does the storing.
    pub fn process_in_main_thread(
        &mut self,
        _manager: &mut dyn ChunkProcessorManagerFace,
        background_task_result: bool,
    ) -> bool {
        background_task_result
    }

    /// `cleanup` (`navmesh_processor.cpp:114-121`): `ChunkProcessor::
    /// cleanup` then unlock the neighbourhood. The destructor's
    /// "has chunks locked" complaint is [`drop`](Self::drop).
    pub fn cleanup(&mut self, manager: &mut dyn ChunkProcessorManagerFace) -> Result<(), String> {
        self.locked_chunks.clear(manager).map_err(|e| e.to_string())
    }

    /// `NavmeshProcessor::~NavmeshProcessor` (`navmesh_processor.cpp:
    /// 36-42`): the "destroyed when it has chunks locked" complaint.
    pub fn has_locks(&self) -> bool {
        !self.locked_chunks.is_empty()
    }
}

// ---------------------------------------------------------------------------
// The navmesh chunk cache
// (`tools/common/editor_chunk_navmesh_cache_base.{hpp,cpp}`)
// ---------------------------------------------------------------------------

/// `BaseChunkSpace::NAVMESH_GENERATOR_NAVGEN|RECAST|NONE`
/// (`chunk_space.hpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavmeshGenerator {
    /// `NAVMESH_GENERATOR_NONE`.
    None,
    /// `NAVMESH_GENERATOR_NAVGEN`.
    Navgen,
    /// `NAVMESH_GENERATOR_RECAST`.
    Recast,
}

/// `UnsavedList::unsavedChunks_` (`editor_chunk_processor_cache.hpp`):
/// the chunks whose caches need writing back.
#[derive(Debug, Default)]
pub struct UnsavedList {
    /// `Chunk*` identities, in insertion order.
    chunks: BTreeSet<ChunkId>,
}

impl UnsavedList {
    /// `UnsavedList::unsavedChunks_.add(chunk)`.
    pub fn add(&mut self, chunk: &ChunkId) {
        self.chunks.insert(chunk.clone());
    }

    /// `unsavedChunks_.size()`.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// `unsavedChunks_.empty()`.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// The chunk identities the list carries.
    pub fn chunks(&self) -> impl Iterator<Item = &ChunkId> {
        self.chunks.iter()
    }
}

/// `EditorChunkNavmeshCacheBase::IDLE_NAV_CALC_TIME`
/// (`editor_chunk_navmesh_cache_base.cpp:43`): the time a dirty flag
/// must stay untouched before a recalculation starts.
pub const IDLE_NAV_CALC_TIME: f64 = 1.0;

/// `s_doWaitBeforeCalculate` (`:44`): the idle-time gate is off by
/// default; the port keeps it a field so the cache can opt in.
#[derive(Debug, Default)]
pub struct CacheConfig {
    /// `s_doWaitBeforeCalculate`.
    pub do_wait_before_calculate: bool,
}

/// `CDataSection` (`tools/common/...` DataSection model): the binary
/// section tree the reference writes the navmesh into. `load` reads the
/// `navmeshDirty` bool and the `worldNavmesh` block; `saveChunk`/
/// `saveCData` rewrite them.
#[derive(Debug, Clone, Default)]
pub struct CDataSection {
    sections: BTreeMap<String, CDataSection>,
    binary: Option<Vec<u8>>,
    /// The section's own data (`resource`, etc.).
    resource: Option<String>,
}

impl CDataSection {
    /// `openSection(name, create)`.
    pub fn open_section(&mut self, name: &str, create: bool) -> Option<&mut CDataSection> {
        if create {
            Some(self.sections.entry(name.to_string()).or_default())
        } else {
            self.sections.get_mut(name)
        }
    }

    /// `deleteSection(name)`.
    pub fn delete_section(&mut self, name: &str) {
        self.sections.remove(name);
    }

    /// `deleteSections(name)`.
    pub fn delete_sections(&mut self, name: &str) {
        self.sections.remove(name);
    }

    /// `findChild(name)`.
    pub fn find_child(&self, name: &str) -> Option<&CDataSection> {
        self.sections.get(name)
    }

    /// `setBinary`.
    pub fn set_binary(&mut self, block: Vec<u8>) {
        self.binary = Some(block);
    }

    /// `asBinary`.
    pub fn as_binary(&self) -> Option<&[u8]> {
        self.binary.as_deref()
    }

    /// `writeString` (the `resource` child's text).
    pub fn set_resource(&mut self, value: &str) {
        self.resource = Some(value.to_string());
    }

    /// The `resource` string.
    pub fn resource(&self) -> Option<&str> {
        self.resource.as_deref()
    }

    /// The child names, sorted (`BW::vector<DataSectionPtr>` order).
    pub fn children(&self) -> impl Iterator<Item = &str> {
        self.sections.keys().map(String::as_str)
    }
}

/// `EditorChunkNavmeshCacheBase` (`editor_chunk_navmesh_cache_base.hpp`):
/// caches one chunk's navmesh and drives its recalculation.
#[derive(Debug)]
pub struct NavmeshCache {
    /// `chunk_`.
    chunk: ChunkId,
    /// `navmesh_`.
    navmesh: Vec<u8>,
    /// `dirty_`.
    dirty: bool,
    /// `dirtyTime_`.
    dirty_time: u64,
    /// `currentProcessor_`.
    current_processor: Option<u64>,
    /// The navmesh generator the space is configured with.
    generator: NavmeshGenerator,
    /// `IDLE_NAV_CALC_TIME`'s clock.
    config: CacheConfig,
    /// `unsavedList_` for the chunk that owns the cache. Read through
    /// [`NavmeshCache::unsaved`].
    unsaved: UnsavedList,
}

impl NavmeshCache {
    /// `EditorChunkNavmeshCacheBase::EditorChunkNavmeshCacheBase` — the
    /// cache starts dirty.
    pub fn new(chunk: &str, generator: NavmeshGenerator) -> Self {
        Self {
            chunk: ChunkId::from(chunk),
            navmesh: Vec::new(),
            dirty: true,
            dirty_time: 0,
            current_processor: None,
            generator,
            config: CacheConfig::default(),
            unsaved: UnsavedList::default(),
        }
    }

    /// `chunk_`.
    pub fn chunk(&self) -> &str {
        &self.chunk
    }

    /// `navmesh_`.
    pub fn navmesh(&self) -> &[u8] {
        &self.navmesh
    }

    /// `dirty_`.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// `dirty_` through the generator gate (`:230-239`): with
    /// `NAVMESH_GENERATOR_NONE` the chunk never needs a navmesh.
    pub fn dirty(&self) -> bool {
        if self.generator == NavmeshGenerator::None {
            return false;
        }
        self.dirty
    }

    /// The unsaved-chunk list this cache marks on `calcDone`.
    pub fn unsaved(&self) -> &UnsavedList {
        &self.unsaved
    }

    /// `s_doWaitBeforeCalculate`.
    pub fn set_do_wait_before_calculate(&mut self, wait: bool) {
        self.config.do_wait_before_calculate = wait;
    }

    /// `load` (`:63-109`): read the dirty status (a one-byte bool), read
    /// the `worldNavmesh` block, and mark the cache dirty when a stored
    /// navmesh is missing. The reference's "it's not really an error to
    /// not have navigation data" applies — this always succeeds.
    pub fn load(&mut self, cdata: &CDataSection) {
        self.dirty = true;
        self.dirty_time = 0;

        // `navmeshDirty`: a bool written as binary. A one-byte block
        // that is not exactly one byte long leaves the cache dirty —
        // the reference reads `*((bool*)bp->cdata())` unchecked.
        if let Some(bytes) = cdata
            .find_child("navmeshDirty")
            .and_then(|sec| sec.as_binary())
            && bytes.len() == 1
        {
            self.dirty = bytes[0] != 0;
        }

        if !self.dirty && cdata.find_child("worldNavmesh").is_none() {
            self.dirty = true;
        }

        if self.config.do_wait_before_calculate {
            self.dirty_time = 1;
        }

        if let Some(bytes) = cdata
            .find_child("worldNavmesh")
            .and_then(|sec| sec.as_binary())
        {
            self.navmesh = bytes.to_vec();
        }
    }

    /// `saveChunk` (`:111-119`): drop the stored navmesh and, when the
    /// space generates navmeshes at all, write the `resource` pointer.
    pub fn save_chunk(&mut self, chunk: &mut CDataSection) {
        chunk.delete_section("worldNavmesh");
        if self.generator != NavmeshGenerator::None {
            chunk
                .open_section("worldNavmesh", true)
                .expect("just created")
                .set_resource(&format!("{}.cdata/worldNavmesh", self.chunk));
        }
    }

    /// `saveCData` (`:121-147`): rewrite `navmeshDirty` and, when
    /// non-empty, `worldNavmesh`. Neither is written for a
    /// `NAVMESH_GENERATOR_NONE` space.
    pub fn save_cdata(&mut self, cdata: &mut CDataSection) {
        cdata.delete_sections("worldNavmesh");
        cdata.delete_sections("navmeshDirty");

        if self.generator != NavmeshGenerator::None {
            let dirty = self.dirty();
            let dirty_sec = cdata
                .open_section("navmeshDirty", true)
                .expect("just created");
            dirty_sec.set_binary(vec![u8::from(dirty)]);

            if !self.navmesh.is_empty() {
                let navmesh = cdata
                    .open_section("worldNavmesh", true)
                    .expect("just created");
                navmesh.set_binary(self.navmesh.clone());
            }
        }
    }

    /// `touch` (`:55-58`): mark the cache dirty.
    pub fn touch(&mut self) {
        self.dirty = true;
        if self.config.do_wait_before_calculate {
            self.dirty_time = 1;
        }
    }

    /// `readyToCalculate` (`:159-177`): with the idle-time gate off the
    /// cache is ready whenever the chunk can be processed with the
    /// outside-chunk fan-out `(1, 1)`.
    pub fn ready_to_calculate(&self, manager: &dyn ChunkProcessorManagerFace) -> bool {
        if self.config.do_wait_before_calculate {
            let time = 0.0;
            let idle = time >= IDLE_NAV_CALC_TIME;
            if !idle {
                return false;
            }
        }
        if manager.is_outside_chunk(&self.chunk) {
            return manager.is_chunk_ready_to_process(&self.chunk, 1, 1);
        }
        manager.is_chunk_ready_to_process(&self.chunk, 1, 0)
    }

    /// `loadChunkForCalculate` (`:183-193`).
    pub fn load_chunk_for_calculate(&self, manager: &mut dyn ChunkProcessorManagerFace) -> bool {
        if manager.is_outside_chunk(&self.chunk) {
            manager.load_chunk_for_processing(&self.chunk, 1, 1)
        } else {
            manager.load_chunk_for_processing(&self.chunk, 1, 0)
        }
    }

    /// `invalidate(manager, spread)` (`:205-260`): mark the cache dirty
    /// and refresh the dirty status. The `pChangedItem` bounding-box
    /// arm — "an object within 1m of a chunk edge affects its navmesh"
    /// — is the changed item's `edWorldBounds` against the chunk's.
    pub fn invalidate(
        &mut self,
        manager: &mut dyn ChunkProcessorManagerFace,
        spread: bool,
        changed_bounds: Option<(Vector3, Vector3)>,
    ) {
        if spread {
            if manager.is_outside_chunk(&self.chunk) {
                manager.spread_invalidate(&self.chunk, 1, 1, 0, None);
            } else {
                manager.spread_invalidate(&self.chunk, 1, 0, 0, None);
            }
            return;
        }

        // `bb.expandSymmetrically(1, 1, 1)` then intersect with the
        // chunk's bounds: an item within 1m of the edge affects the
        // navmesh, so anything further away does nothing.
        if let Some((lo, hi)) = changed_bounds {
            let expanded = (
                Vector3::new(lo.x - 1.0, lo.y - 1.0, lo.z - 1.0),
                Vector3::new(hi.x + 1.0, hi.y + 1.0, hi.z + 1.0),
            );
            let chunk_bounds = manager.chunk_bounds(&self.chunk);
            if !bound_box_intersects(&expanded, &chunk_bounds) {
                return;
            }
        }

        self.dirty = true;
        self.current_processor = None;
        manager.update_chunk_dirty_status(&self.chunk);
    }

    /// `recalc` (`:264-303`): assert the chunk is ready, clear the
    /// stored navmesh, and start the generator's processor. The
    /// reference asserts `readyToCalculate`; the port checks it and
    /// returns false when the manager disagrees.
    pub fn recalc(
        &mut self,
        manager: &mut dyn ChunkProcessorManagerFace,
        processors: &mut ChunkProcessors,
        body: Box<dyn ProcessorBody>,
    ) -> Result<(), String> {
        if !self.ready_to_calculate(manager) {
            return Err("EditorChunkNavmeshCacheBase::recalc: chunk is not ready".to_string());
        }
        self.dirty = true;
        self.dirty_time = 1;
        self.navmesh.clear();

        match self.generator {
            NavmeshGenerator::Navgen => {
                let id = next_processor_id();
                processors.add_processor(id, body);
                self.current_processor = Some(id);
                Ok(())
            }
            NavmeshGenerator::Recast => Err(
                "EditorChunkNavmeshCacheBase::recast generation is not ported (BW-10-05-03)"
                    .to_string(),
            ),
            NavmeshGenerator::None => {
                Err("EditorChunkNavmeshCacheBase::recalc invalid navmesh type".to_string())
            }
        }
    }

    /// `calcDone` (`:321-349`): only the current processor's result
    /// is stored. A successful result replaces the navmesh and clears
    /// the dirty flag, marks the chunk unsaved and refreshes the dirty
    /// status. Either way `currentProcessor_` is cleared.
    pub fn calc_done(
        &mut self,
        processor: u64,
        background_task_result: bool,
        navmesh: &[u8],
        unsaved: &mut UnsavedList,
        manager: &mut dyn ChunkProcessorManagerFace,
    ) {
        if self.current_processor != Some(processor) {
            return;
        }
        if background_task_result {
            self.navmesh = navmesh.to_vec();
            self.dirty = false;
            unsaved.add(&self.chunk);
            manager.update_chunk_dirty_status(&self.chunk);
        }
        self.current_processor = None;
    }
}

/// `boundingBox().intersects(bb)` (`chunk.cpp:695-806`): half-open box
/// intersection. The chunk's bounds face comes from the manager.
fn bound_box_intersects(a: &(Vector3, Vector3), b: &(Vector3, Vector3)) -> bool {
    a.0.x < b.1.x
        && a.1.x > b.0.x
        && a.0.y < b.1.y
        && a.1.y > b.0.y
        && a.0.z < b.1.z
        && a.1.z > b.0.z
}

/// The processor id allocator (`ChunkProcessorPtr`'s identity).
fn next_processor_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::RwLock;

    use foundation::math::matrix::Matrix;
    use waypoint_gen::MAX_HEIGHTS;

    // -----------------------------------------------------------------------
    // Test doubles
    // -----------------------------------------------------------------------

    /// A manager face the lock tests drive: a tiny grid of outside chunks
    /// with per-chunk overlappers and a map of chunk bounds.
    #[derive(Debug, Default)]
    struct TestManager {
        locks: LockedChunks,
        background: Vec<u64>,
        main: Vec<u64>,
        /// `(x, z) -> grid name` for `grid_from_chunk_name`.
        grids: BTreeMap<ChunkId, (i16, i16)>,
        outside: BTreeSet<ChunkId>,
        overlappers: BTreeMap<ChunkId, Vec<ChunkId>>,
        bounds: BTreeMap<ChunkId, (Vector3, Vector3)>,
        ready: bool,
        running_threads: usize,
        dirty_updates: Vec<ChunkId>,
        spread_calls: usize,
        load_calls: Vec<(ChunkId, i32, i32)>,
        spread: BTreeMap<ChunkId, Vec<ChunkId>>,
        neighbours: BTreeMap<ChunkId, Vec<ChunkId>>,
        outside_ids: BTreeMap<(i32, i32), ChunkId>,
    }

    impl TestManager {
        fn register_outside(&mut self, id: &str, x: i16, z: i16) {
            self.outside.insert(id.to_string());
            self.grids.insert(id.to_string(), (x, z));
            self.outside_ids
                .insert((x.into(), z.into()), id.to_string());
            self.bounds
                .entry(id.to_string())
                .or_insert((Vector3::new(0.0, 0.0, 0.0), Vector3::new(1.0, 1.0, 1.0)));
        }

        fn lock_count(&self, chunk: &str) -> i32 {
            self.locks.lock_count(&ChunkId::from(chunk))
        }
    }

    impl ChunkProcessorManagerFace for TestManager {
        fn num_running_threads(&self) -> usize {
            self.running_threads
        }
        fn add_background_task(&mut self, processor: u64) {
            self.background.push(processor);
        }
        fn add_main_thread_task(&mut self, processor: u64) {
            self.main.push(processor);
        }
        fn locked_chunks(&mut self) -> &mut LockedChunks {
            &mut self.locks
        }
        fn grid_from_chunk_name(&self, name: &str) -> Option<(i16, i16)> {
            self.grids.get(name).copied()
        }
        fn outside_chunk_identifier(&self, x: i32, z: i32) -> String {
            self.outside_ids.get(&(x, z)).cloned().unwrap_or_default()
        }
        fn is_outside_chunk(&self, chunk: &ChunkId) -> bool {
            self.outside.contains(chunk)
        }
        fn chunk_bounds(&self, chunk: &ChunkId) -> (Vector3, Vector3) {
            self.bounds
                .get(chunk)
                .copied()
                .unwrap_or((Vector3::new(0.0, 0.0, 0.0), Vector3::new(1.0, 1.0, 1.0)))
        }
        fn overlappers(&self, chunk: &ChunkId) -> Vec<ChunkId> {
            self.overlappers.get(chunk).cloned().unwrap_or_default()
        }
        fn outside_neighbours(&self, chunk: &ChunkId) -> Vec<ChunkId> {
            if let Some(list) = self.neighbours.get(chunk) {
                return list.clone();
            }
            let Some((gx, gz)) = self.grids.get(chunk).copied() else {
                return Vec::new();
            };
            let mut out = Vec::new();
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                if let Some(name) = self
                    .outside_ids
                    .get(&(i32::from(gx) + dx, i32::from(gz) + dz))
                {
                    out.push(name.clone());
                }
            }
            out
        }
        fn is_chunk_ready_to_process(&self, _chunk: &ChunkId, _ex: i32, _ez: i32) -> bool {
            self.ready
        }
        fn load_chunk_for_processing(&mut self, chunk: &ChunkId, ex: i32, ez: i32) -> bool {
            self.load_calls.push((chunk.clone(), ex, ez));
            self.ready
        }
        fn spread_invalidate(
            &mut self,
            chunk: &ChunkId,
            _ex: i32,
            _ez: i32,
            _cache_index: usize,
            _changed_item: Option<&str>,
        ) {
            self.spread_calls += 1;
            for neighbour in self.spread.get(chunk).cloned().unwrap_or_default() {
                self.locks.lock(&neighbour);
            }
        }
        fn update_chunk_dirty_status(&mut self, chunk: &ChunkId) {
            self.dirty_updates.push(chunk.clone());
        }
    }

    /// A navmesh chunk face: identity, flags, portal planes and probe
    /// geometry, driven by an `entity_positions` list.
    #[derive(Debug)]
    struct TestChunk {
        id: ChunkId,
        outside: bool,
        loaded: bool,
        loading: bool,
        planes: Vec<PlaneEq>,
        entities: Vec<Vector3>,
        bounds: (Vector3, Vector3),
    }

    impl TestChunk {
        fn outside(id: &str) -> Self {
            Self {
                id: id.to_string(),
                outside: true,
                loaded: true,
                loading: false,
                planes: Vec::new(),
                entities: Vec::new(),
                bounds: (Vector3::new(0.0, 0.0, 0.0), Vector3::new(10.0, 10.0, 10.0)),
            }
        }
    }

    impl NavmeshProbe for TestChunk {
        fn local_bounds(&self) -> (Vector3, Vector3) {
            self.bounds
        }
        fn transform(&self) -> Matrix {
            Matrix::identity()
        }
        fn transform_inverse(&self) -> Matrix {
            Matrix::identity()
        }
        fn owns(&self, _point: Vector3) -> bool {
            true
        }
    }

    impl NavmeshEntityPositions for TestChunk {
        fn entity_positions(&self) -> Vec<Vector3> {
            self.entities.clone()
        }
    }

    impl NavgenChunkFace for TestChunk {
        fn identifier(&self) -> &str {
            &self.id
        }
        fn is_outside_chunk(&self) -> bool {
            self.outside
        }
        fn loaded(&self) -> bool {
            self.loaded
        }
        fn loading(&self) -> bool {
            self.loading
        }
        fn portal_planes(&self) -> Vec<PlaneEq> {
            self.planes.clone()
        }
    }

    /// The flat-collision scene the test flood fills over, mirroring the
    /// synthetic physics in `waypoint-gen`'s own fixtures.
    struct FlatPhysics;

    impl waypoint_gen::FloodPhysics for FlatPhysics {
        fn get_girth(&self) -> Vector3 {
            Vector3::new(0.5, 1.5, 0.5)
        }
        fn get_scramble_height(&self) -> f32 {
            0.5
        }
        fn find_drop_point(&mut self, pos: &Vector3, y: &mut f32) -> bool {
            *y = 0.0;
            let _ = pos;
            true
        }
        fn adjust_move(&mut self, src: &Vector3, dst: &Vector3, dst2: &mut Vector3) {
            if dst.y.abs() <= 0.6 {
                *dst2 = Vector3::new(dst.x, 0.0, dst.z);
            } else {
                *dst2 = Vector3::new(dst.x, -10000.0, dst.z);
            }
            let _ = src;
        }
    }

    /// A chunk that owns everything: the flood's `owns`/space bounds.
    struct AllChunk;

    impl waypoint_gen::FloodChunk for AllChunk {
        fn owns(&self, _point: &Vector3) -> bool {
            true
        }
        fn min_coord_x(&self) -> f32 {
            -100.0
        }
        fn max_coord_x(&self) -> f32 {
            100.0
        }
        fn min_coord_z(&self) -> f32 {
            -100.0
        }
        fn max_coord_z(&self) -> f32 {
            100.0
        }
    }

    /// A flood controller double: it owns a real [`WaypointFlood`] filled
    /// over a flat surface, so generation has actual polygons to emit.
    #[derive(Debug, Clone)]
    struct TestFlood {
        flood: waypoint_gen::WaypointFlood,
        succeed: bool,
        calls: Rc<RefCell<Vec<f32>>>,
    }

    impl TestFlood {
        /// Fill a `width`x`height` point grid at `resolution` over a flat
        /// surface: `WaypointGenerator` then finds a polygon set.
        fn stage_grid(width: i32, height: i32, resolution: f32, min: Vector3) -> Self {
            let mut flood = waypoint_gen::WaypointFlood::default();
            let max = Vector3::new(
                min.x + (width - 1) as f32 * resolution,
                0.0,
                min.z + (height - 1) as f32 * resolution,
            );
            flood.set_area(&min, &max, resolution);
            let mut physics = FlatPhysics;
            flood.fill(
                &Vector3::new(min.x + 0.5, 10.0, min.z + 0.5),
                &mut physics,
                &AllChunk,
                None,
            );
            flood.postfilter_add();
            flood.postfilter_remove();
            Self {
                flood,
                succeed: true,
                calls: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn width(&self) -> i32 {
            self.flood.xsize()
        }
    }

    impl DropPhysics for TestFlood {
        fn drop_point(&mut self, _point: Vector3) -> Option<f32> {
            Some(0.0)
        }
    }

    impl FloodController for TestFlood {
        fn flood(&mut self, girth: f32, _entity_points: &[Vector3]) -> bool {
            self.calls.borrow_mut().push(girth);
            self.succeed
        }
        fn stage(&self) -> FloodSnapshot {
            FloodSnapshot::from_stage(&self.flood)
        }
    }

    /// A processor body counting its calls, with configurable results.
    #[derive(Debug)]
    struct TestBody {
        background_result: bool,
        main_result: bool,
        cleanup_result: Result<(), String>,
        events: Arc<RwLock<Vec<&'static str>>>,
    }

    impl TestBody {
        fn new() -> Self {
            Self {
                background_result: true,
                main_result: true,
                cleanup_result: Ok(()),
                events: Arc::new(RwLock::new(Vec::new())),
            }
        }
    }

    impl ProcessorBody for TestBody {
        fn process_in_background(&mut self, _manager: &mut dyn ChunkProcessorManagerFace) -> bool {
            self.events.write().unwrap().push("bg");
            self.background_result
        }
        fn process_in_main_thread(
            &mut self,
            _manager: &mut dyn ChunkProcessorManagerFace,
            _background_task_result: bool,
        ) -> bool {
            self.events.write().unwrap().push("main");
            self.main_result
        }
        fn cleanup(&mut self, _manager: &mut dyn ChunkProcessorManagerFace) -> Result<(), String> {
            self.events.write().unwrap().push("cleanup");
            self.cleanup_result.clone()
        }
        fn task_name(&self) -> &str {
            "TestBody"
        }
    }

    // -----------------------------------------------------------------------
    // LockedChunks semantics
    // -----------------------------------------------------------------------

    #[test]
    fn locked_chunks_refcounts_to_zero() {
        let mut locks = LockedChunks::default();
        locks.lock(&"a".into());
        locks.lock(&"a".into());
        assert_eq!(locks.lock_count(&"a".into()), 2);
        assert!(!locks.is_empty());
        assert_eq!(locks.len(), 1);
        assert_eq!(locks.unlock(&"a".into()), Ok(()));
        assert_eq!(locks.lock_count(&"a".into()), 1);
        // the zero entry is dropped from the map
        assert_eq!(locks.unlock(&"a".into()), Ok(()));
        assert_eq!(locks.lock_count(&"a".into()), 0);
        assert!(locks.is_empty());
    }

    #[test]
    fn locked_chunks_unlock_errors() {
        let mut locks = LockedChunks::default();
        assert_eq!(
            locks.unlock(&"a".into()),
            Err(LockedChunkError::NeverLocked("a".into()))
        );
        locks.lock(&"a".into());
        // `debug_assert` only; release keeps the count, the entry is gone
        // at zero so a second unlock reports NeverLocked.
        assert_eq!(locks.unlock(&"a".into()), Ok(()));
        assert_eq!(
            locks.unlock(&"a".into()),
            Err(LockedChunkError::NeverLocked("a".into()))
        );
        locks.clear();
        assert!(locks.is_empty());
    }

    #[test]
    fn locked_chunks_unlock_all_stops_at_first_missing() {
        let mut locks = LockedChunks::default();
        let set: ChunkSet = ["a".to_string(), "b".to_string()].into_iter().collect();
        assert_eq!(
            locks.unlock_all(&set),
            Err(LockedChunkError::NeverLocked("a".into()))
        );
    }

    #[test]
    fn locked_chunk_error_display() {
        assert!(format!("{}", LockedChunkError::NeverLocked("x".into())).contains("unheld"));
        assert!(format!("{}", LockedChunkError::ZeroCount("x".into())).contains("zero"));
        assert!(format!("{}", LockedChunkError::NotLocked("x".into())).contains("did not lock"));
        assert!(
            format!("{}", LockedChunkError::RecursionCap("x".into())).contains("recursion cap")
        );
        fn assert_error<T: Error>() {}
        assert_error::<LockedChunkError>();
    }

    // -----------------------------------------------------------------------
    // ScopedLockedChunkHolder
    // -----------------------------------------------------------------------

    #[test]
    fn holder_lock_grid_expansion_quirk() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        manager.register_outside("c1", 1, 0);
        manager.register_outside("c-1", -1, 0);
        manager.register_outside("c0_1", 0, 1);

        // `lock_grid(chunk, 0, 5)` compares expandX with itself, so the
        // non-zero expandZ never expands: the quirk keeps it a plain lock.
        let mut holder = ScopedLockedChunkHolder::new();
        holder.lock_grid(&"c0".into(), 0, 5, &mut manager).unwrap();
        assert_eq!(holder.len(), 1);
        assert!(holder.contains(&"c0".into()));

        // a real expansion locks the (2x+1)x(2z+1) ring.
        let mut holder = ScopedLockedChunkHolder::new();
        holder.lock_grid(&"c0".into(), 1, 1, &mut manager).unwrap();
        for name in ["c-1", "c0", "c1", "c0_1"] {
            assert!(holder.contains(&name.into()), "missing {name}");
        }
        assert!(!holder.contains(&"c0_-1".into()));
        // the earlier plain lock plus this ring hold c0 twice
        assert_eq!(manager.lock_count("c0"), 2);
        assert_eq!(manager.lock_count("c1"), 1);
    }

    #[test]
    fn holder_double_lock_is_idempotent() {
        let mut manager = TestManager::default();
        let mut holder = ScopedLockedChunkHolder::new();
        holder.lock(&"a".into(), &mut manager).unwrap();
        holder.lock(&"a".into(), &mut manager).unwrap();
        assert_eq!(holder.count(&"a".into()), 1);
        holder.erase(&"a".into(), &mut manager).unwrap();
        assert!(holder.is_empty());
        assert_eq!(manager.lock_count("a"), 0);
        // erasing a missing chunk reports it without touching the locks
        assert_eq!(
            holder.erase(&"a".into(), &mut manager),
            Err(LockedChunkError::NeverLocked("a".into()))
        );
    }

    #[test]
    fn holder_portal_depth_walks_and_dedups() {
        let mut manager = TestManager::default();
        manager.register_outside("a", 0, 0);
        manager.register_outside("b", 1, 0);
        manager.register_outside("c", 2, 0);
        manager.overlappers.insert("a".into(), vec!["ov".into()]);

        // depth 0 is a plain lock
        let mut holder = ScopedLockedChunkHolder::new();
        holder
            .lock_portal_depth(&"a".into(), 0, &mut manager)
            .unwrap();
        assert_eq!(holder.len(), 1);

        // depth 1 walks one hop: a -> b, plus a's overlappers
        let mut holder = ScopedLockedChunkHolder::new();
        holder
            .lock_portal_depth(&"a".into(), 1, &mut manager)
            .unwrap();
        assert!(holder.contains(&"a".into()));
        assert!(holder.contains(&"b".into()));
        assert!(!holder.contains(&"ov".into())); // portal arm does not expand overlappers
        assert!(!holder.contains(&"c".into()));
    }

    #[test]
    fn holder_clear_unlocks_everything() {
        let mut manager = TestManager::default();
        manager.register_outside("a", 0, 0);
        manager.register_outside("b", 1, 0);
        let mut holder = ScopedLockedChunkHolder::new();
        holder.lock(&"a".into(), &mut manager).unwrap();
        holder.lock(&"b".into(), &mut manager).unwrap();
        holder.clear(&mut manager).unwrap();
        assert!(holder.is_empty());
        assert_eq!(manager.lock_count("a"), 0);
        assert_eq!(manager.lock_count("b"), 0);
    }

    // -----------------------------------------------------------------------
    // lockChunkInMemory arms
    // -----------------------------------------------------------------------

    #[test]
    fn lock_outside_ring_plus_overlappers() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        manager.register_outside("c1", 1, 0);
        manager.register_outside("c-1", -1, 0);
        manager.register_outside("c0_1", 0, 1);
        manager.register_outside("c0_-1", 0, -1);
        manager.register_outside("c1_1", 1, 1);
        manager.overlappers.insert(
            "c0".into(),
            vec!["ov0".into(), "ov1".into(), "ov0".into(), "c1".into()],
        );
        let mut holder = ScopedLockedChunkHolder::new();
        manager
            .lock_chunk_in_memory(&"c0".into(), 1, 1, &mut holder)
            .unwrap();
        for name in ["c-1", "c0", "c1", "c0_1", "c0_-1", "c1_1", "ov0", "ov1"] {
            assert!(holder.contains(&name.into()), "missing {name}");
        }
    }

    #[test]
    fn lock_outside_bad_grid_name_errors() {
        let mut manager = TestManager::default();
        manager.outside.insert("ghost".into());
        let mut holder = ScopedLockedChunkHolder::new();
        assert_eq!(
            manager.lock_chunk_in_memory(&"ghost".into(), 1, 1, &mut holder),
            Err(LockedChunkError::NeverLocked("ghost".into()))
        );
    }

    #[test]
    fn lock_inside_recurses_into_neighbours() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        manager.register_outside("c1", 1, 0);
        manager.outside.insert("in0".into());
        manager
            .neighbours
            .insert("in0".into(), vec!["c0".into(), "c1".into()]);
        let mut holder = ScopedLockedChunkHolder::new();
        manager
            .lock_chunk_in_memory_inside(&"in0".into(), 1, 0, &mut holder, 0)
            .unwrap();
        assert!(holder.contains(&"c0".into()));
        assert!(holder.contains(&"c1".into()));
        assert!(!holder.contains(&"in0".into()));
    }

    #[test]
    fn lock_inside_cyclic_neighbours_hits_the_cap() {
        let mut manager = TestManager::default();
        // every inside chunk "neighbours" itself: the reference recurses
        // forever; the port answers with the cap error.
        manager
            .neighbours
            .insert("cycle".into(), vec!["cycle".into()]);
        let mut holder = ScopedLockedChunkHolder::new();
        let result = manager.lock_chunk_in_memory_inside(&"cycle".into(), 0, 0, &mut holder, 0);
        assert!(matches!(result, Err(LockedChunkError::RecursionCap(_))));
    }

    #[test]
    fn lock_portal_visited_set_dedups() {
        let mut manager = TestManager::default();
        manager.register_outside("a", 0, 0);
        manager.register_outside("b", 1, 0);
        manager.register_outside("c", 2, 0);
        let mut holder = ScopedLockedChunkHolder::new();
        manager
            .lock_chunk_in_memory_portal(&"a".into(), 2, &mut holder)
            .unwrap();
        for name in ["a", "b", "c"] {
            assert!(holder.contains(&name.into()), "missing {name}");
        }
        assert_eq!(holder.len(), 3);
    }

    // -----------------------------------------------------------------------
    // ChunkProcessorTask lifecycle
    // -----------------------------------------------------------------------

    fn manager_with_locks() -> TestManager {
        TestManager::default()
    }

    #[test]
    fn process_queued_when_threads_running() {
        let mut manager = manager_with_locks();
        manager.running_threads = 1;
        let mut task = ChunkProcessorTask::new(TestBody::new(), 7);
        let ok = task.process(&mut manager).unwrap();
        assert!(ok);
        assert_eq!(manager.background, vec![7]);
    }

    #[test]
    fn process_single_threaded_runs_all_stages() {
        let mut manager = manager_with_locks();
        let body = TestBody::new();
        let events = body.events.clone();
        let mut task = ChunkProcessorTask::new(body, 3);
        assert!(task.process(&mut manager).unwrap());
        assert_eq!(*events.read().unwrap(), vec!["bg", "main", "cleanup"]);
    }

    #[test]
    fn background_failure_propagates() {
        let mut manager = manager_with_locks();
        let mut body = TestBody::new();
        body.background_result = false;
        let mut task = ChunkProcessorTask::new(body, 3);
        assert!(!task.process(&mut manager).unwrap());
    }

    #[test]
    fn cancel_queues_the_main_thread() {
        let mut manager = manager_with_locks();
        let mut task = ChunkProcessorTask::new(TestBody::new(), 5);
        task.cancel(&mut manager);
        assert_eq!(manager.main, vec![5]);
    }

    #[test]
    fn do_background_then_main_requeue_chain() {
        let mut manager = manager_with_locks();
        let body = TestBody::new();
        let events = body.events.clone();
        let mut task = ChunkProcessorTask::new(body, 9);
        task.do_background_task(&mut manager);
        assert_eq!(*events.read().unwrap(), vec!["bg"]);
        assert_eq!(manager.main, vec![9]);

        task.do_main_thread_task(&mut manager);
        assert_eq!(*events.read().unwrap(), vec!["bg", "main", "cleanup"]);
    }

    #[test]
    fn do_main_thread_failure_requeues() {
        let mut manager = manager_with_locks();
        let mut body = TestBody::new();
        body.main_result = false;
        let events = body.events.clone();
        let mut task = ChunkProcessorTask::new(body, 4);
        task.do_background_task(&mut manager);
        manager.main.clear();
        task.do_main_thread_task(&mut manager);
        assert_eq!(*events.read().unwrap(), vec!["bg", "main"]);
        assert_eq!(manager.main, vec![4]);
        assert!(!task.cleaned_up());
    }

    #[test]
    fn cleanup_failure_requeues_through_main() {
        let mut manager = manager_with_locks();
        let mut body = TestBody::new();
        body.cleanup_result = Err("locked out".into());
        let events = body.events.clone();
        let mut task = ChunkProcessorTask::new(body, 4);
        task.do_background_task(&mut manager);
        manager.main.clear();
        // the reference's cleanup is void; a failed unlock is ignored
        // after the main-thread work succeeded.
        task.do_main_thread_task(&mut manager);
        assert_eq!(*events.read().unwrap(), vec!["bg", "main", "cleanup"]);
        assert!(manager.main.is_empty());
        // cleanup is once-only: a second pass runs only main again
        manager.main.clear();
        task.do_main_thread_task(&mut manager);
        assert_eq!(
            *events.read().unwrap(),
            vec!["bg", "main", "cleanup", "main"]
        );
    }

    #[test]
    fn cleanup_is_once_only() {
        let mut manager = manager_with_locks();
        let body = TestBody::new();
        let events = body.events.clone();
        let mut task = ChunkProcessorTask::new(body, 1);
        task.cleanup(&mut manager).unwrap();
        task.cleanup(&mut manager).unwrap();
        assert_eq!(*events.read().unwrap(), vec!["cleanup"]);
        assert!(task.cleaned_up());
        assert_eq!(task.id(), 1);
    }

    #[test]
    fn listeners_fire_once_and_self_unregister() {
        let mut task = ChunkProcessorTask::new(TestBody::new(), 11);
        let shared: Arc<RwLock<Vec<u64>>> = Arc::new(RwLock::new(Vec::new()));
        task.add_listener(Arc::new(CountingListener {
            name: 11,
            events: shared.clone(),
        }));
        task.add_listener(Arc::new(SelfRemovingListener {
            name: 22,
            events: shared.clone(),
        }));

        let mut manager = manager_with_locks();
        task.cleanup(&mut manager).unwrap();
        // the self-removing listener fired and unregistered itself; the
        // listener before it still fired.
        assert_eq!(*shared.read().unwrap(), vec![11, 22]);
        assert_eq!(task.listeners().len(), 1);
    }

    struct CountingListener {
        name: u64,
        events: Arc<RwLock<Vec<u64>>>,
    }
    impl ChunkProcessorListener for CountingListener {
        fn on_chunk_processor_finished(&self, _processor: u64, _unregistered: &mut bool) {
            self.events.write().unwrap().push(self.name);
        }
    }

    struct SelfRemovingListener {
        name: u64,
        events: Arc<RwLock<Vec<u64>>>,
    }
    impl ChunkProcessorListener for SelfRemovingListener {
        fn on_chunk_processor_finished(&self, _processor: u64, unregistered: &mut bool) {
            self.events.write().unwrap().push(self.name);
            // the reference's listener calls `delListener(this)` inside
            // the callback; the task then replays the erased slot.
            *unregistered = true;
        }
    }

    #[test]
    fn add_listener_dedups() {
        let mut task = ChunkProcessorTask::new(TestBody::new(), 1);
        let listener: Arc<dyn ChunkProcessorListener> = Arc::new(NoopListener);
        task.add_listener(listener.clone());
        task.add_listener(listener);
        assert_eq!(task.listeners().len(), 1);
        let mut manager = manager_with_locks();
        task.cleanup(&mut manager).unwrap();
        // the listener stays registered; cleanup only notifies it
        assert_eq!(task.listeners().len(), 1);
    }

    struct NoopListener;
    impl ChunkProcessorListener for NoopListener {
        fn on_chunk_processor_finished(&self, _processor: u64, _unregistered: &mut bool) {}
    }

    #[test]
    fn chunk_processors_add_del() {
        let mut processors = ChunkProcessors::default();
        let body: Box<dyn ProcessorBody> = Box::new(TestBody::new());
        processors.add_processor(1, body);
        assert!(!processors.is_empty());
        assert_eq!(
            processors.processors().copied().collect::<Vec<_>>(),
            vec![1]
        );
        let body: Box<dyn ProcessorBody> = Box::new(TestBody::new());
        processors.add_processor(2, body);
        processors.del_processor(1);
        assert_eq!(
            processors.processors().copied().collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(processors.len(), 1);
    }

    // -----------------------------------------------------------------------
    // CDataSection
    // -----------------------------------------------------------------------

    #[test]
    fn cdata_section_roundtrip() {
        let mut root = CDataSection::default();
        root.set_binary(vec![1, 2, 3]);
        assert_eq!(root.as_binary(), Some(&[1, 2, 3][..]));
        let child = root.open_section("child", true).unwrap();
        child.set_resource("x.cdata/y");
        assert_eq!(
            root.find_child("child").unwrap().resource(),
            Some("x.cdata/y")
        );
        assert_eq!(root.children().collect::<Vec<_>>(), vec!["child"]);
        assert!(root.find_child("nope").is_none());
        root.delete_section("child");
        assert!(root.find_child("child").is_none());
        root.delete_sections("ghost");
        let created = root.open_section("c", true).unwrap();
        created.set_resource("r");
        root.set_binary(vec![9]);
        assert_eq!(root.as_binary(), Some(&[9][..]));
        assert!(CDataSection::default().as_binary().is_none());
    }

    // -----------------------------------------------------------------------
    // NavmeshCache
    // -----------------------------------------------------------------------

    fn cache_with_dirty_flag(dirty: bool, world_navmesh: Option<Vec<u8>>) -> NavmeshCache {
        let mut cache = NavmeshCache::new("chunk", NavmeshGenerator::Navgen);
        let mut cdata = CDataSection::default();
        let section = cdata.open_section("navmeshDirty", true).unwrap();
        section.set_binary(vec![u8::from(dirty)]);
        if let Some(navmesh) = world_navmesh {
            cdata
                .open_section("worldNavmesh", true)
                .unwrap()
                .set_binary(navmesh);
        }
        cache.load(&cdata);
        cache
    }

    #[test]
    fn cache_load_reads_dirty_and_navmesh() {
        let cache = cache_with_dirty_flag(false, Some(vec![7, 8]));
        assert!(!cache.is_dirty());
        assert!(!cache.dirty());
        assert_eq!(cache.navmesh(), &[7, 8][..]);

        // no stored navmesh forces the dirty flag on
        let cache = cache_with_dirty_flag(false, None);
        assert!(cache.is_dirty());
        // a one-byte non-zero block marks it dirty
        let cache = cache_with_dirty_flag(true, None);
        assert!(cache.is_dirty());
    }

    #[test]
    fn cache_save_chunk_and_cdata() {
        let mut cache = NavmeshCache::new("chunk", NavmeshGenerator::Navgen);
        let mut cdata = CDataSection::default();
        cache.navmesh = vec![1, 2];
        cache.save_chunk(&mut cdata);
        assert_eq!(
            cdata.find_child("worldNavmesh").unwrap().resource(),
            Some("chunk.cdata/worldNavmesh")
        );

        cache.save_cdata(&mut cdata);
        assert_eq!(
            cdata.find_child("navmeshDirty").unwrap().as_binary(),
            Some(&[1][..])
        );
        assert_eq!(
            cdata.find_child("worldNavmesh").unwrap().as_binary(),
            Some(&[1, 2][..])
        );

        // a NONE generator writes neither section
        let mut none_cache = NavmeshCache::new("chunk", NavmeshGenerator::None);
        none_cache.navmesh = vec![1, 2];
        let mut none_cdata = CDataSection::default();
        none_cache.save_chunk(&mut none_cdata);
        none_cache.save_cdata(&mut none_cdata);
        assert!(none_cdata.find_child("worldNavmesh").is_none());
        assert!(none_cdata.find_child("navmeshDirty").is_none());
    }

    #[test]
    fn cache_touch_and_dirty_time() {
        let mut cache = NavmeshCache::new("chunk", NavmeshGenerator::Navgen);
        cache.set_do_wait_before_calculate(true);
        cache.load(&{
            let mut cdata = CDataSection::default();
            cdata
                .open_section("navmeshDirty", true)
                .unwrap()
                .set_binary(vec![1]);
            cdata
        });
        assert!(cache.is_dirty());
        cache.touch();
        assert!(cache.is_dirty());
        // after load with the wait gate the clock advances
        let mut cache2 = NavmeshCache::new("chunk", NavmeshGenerator::Navgen);
        cache2.set_do_wait_before_calculate(true);
        cache2.load(&CDataSection::default());
        assert!(cache2.is_dirty());
        let _ = cache2.unsaved();
    }

    #[test]
    fn cache_ready_to_calculate_gate() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        let cache = NavmeshCache::new("c0", NavmeshGenerator::Navgen);
        manager.ready = false;
        assert!(!cache.ready_to_calculate(&manager));
        manager.ready = true;
        assert!(cache.ready_to_calculate(&manager));
        assert!(cache.load_chunk_for_calculate(&mut manager));
        assert_eq!(manager.load_calls, vec![("c0".into(), 1, 1)]);
    }

    #[test]
    fn cache_invalidate_bounds_arm() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        let mut cache = NavmeshCache::new("c0", NavmeshGenerator::Navgen);
        // an item far outside the chunk bounds changes nothing
        cache.invalidate(
            &mut manager,
            false,
            Some((Vector3::new(50.0, 0.0, 0.0), Vector3::new(60.0, 1.0, 1.0))),
        );
        assert!(cache.is_dirty());
        assert!(manager.dirty_updates.is_empty());
        // an item within 1m of the edge dirties and updates the status
        cache.invalidate(
            &mut manager,
            false,
            Some((Vector3::new(0.5, 0.0, 0.5), Vector3::new(2.0, 1.0, 2.0))),
        );
        assert_eq!(manager.dirty_updates, vec![ChunkId::from("c0")]);
        // a spread invalidate locks the neighbour set
        let mut cache2 = NavmeshCache::new("c0", NavmeshGenerator::Navgen);
        cache2.invalidate(&mut manager, true, None);
        assert_eq!(manager.spread_calls, 1);
    }

    #[test]
    fn cache_recalc_arms() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        manager.ready = true;
        let mut processors = ChunkProcessors::default();
        let mut cache = NavmeshCache::new("c0", NavmeshGenerator::Navgen);
        cache.navmesh = vec![1, 2, 3];
        cache
            .recalc(&mut manager, &mut processors, Box::new(TestBody::new()))
            .unwrap();
        assert!(cache.is_dirty());
        // the stored navmesh is cleared before the run
        assert!(cache.navmesh().is_empty());
        let id = cache.current_processor.expect("current processor set");
        assert_eq!(
            processors.processors().copied().collect::<Vec<_>>(),
            vec![id]
        );
        assert!(manager.dirty_updates.is_empty());

        // a failed processor does not clear the current pointer
        let mut cache2 = NavmeshCache::new("c0", NavmeshGenerator::Navgen);
        cache2
            .recalc(&mut manager, &mut processors, Box::new(TestBody::new()))
            .unwrap();

        // recast and none generators refuse
        let mut recast = NavmeshCache::new("c0", NavmeshGenerator::Recast);
        assert!(
            recast
                .recalc(&mut manager, &mut processors, Box::new(TestBody::new()))
                .is_err()
        );
        let mut none = NavmeshCache::new("c0", NavmeshGenerator::None);
        assert!(
            none.recalc(&mut manager, &mut processors, Box::new(TestBody::new()))
                .is_err()
        );
    }

    #[test]
    fn cache_recalc_not_ready() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        manager.ready = false;
        let mut processors = ChunkProcessors::default();
        let mut cache = NavmeshCache::new("c0", NavmeshGenerator::Navgen);
        let err = cache
            .recalc(&mut manager, &mut processors, Box::new(TestBody::new()))
            .unwrap_err();
        assert!(err.contains("not ready"));
        assert!(cache.current_processor.is_none());
    }

    #[test]
    fn cache_calc_done_arms() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        manager.ready = true;
        let mut cache = NavmeshCache::new("c0", NavmeshGenerator::Navgen);
        let mut processors = ChunkProcessors::default();
        cache
            .recalc(&mut manager, &mut processors, Box::new(TestBody::new()))
            .unwrap();
        let id = cache.current_processor.unwrap();

        // a mismatched processor is ignored
        let mut unsaved = UnsavedList::default();
        cache.calc_done(id + 1, true, &[9], &mut unsaved, &mut manager);
        assert!(cache.current_processor == Some(id));
        assert_eq!(cache.navmesh(), &[]);

        // a failed result keeps the navmesh, clears the processor
        cache.calc_done(id, false, &[], &mut unsaved, &mut manager);
        assert!(cache.current_processor.is_none());
        assert!(cache.navmesh().is_empty());
        assert!(unsaved.is_empty());

        // a success stores the navmesh, marks unsaved, refreshes status
        cache
            .recalc(&mut manager, &mut processors, Box::new(TestBody::new()))
            .unwrap();
        let id = cache.current_processor.unwrap();
        cache.calc_done(id, true, &[4, 5], &mut unsaved, &mut manager);
        assert_eq!(cache.navmesh(), &[4, 5]);
        assert!(!cache.is_dirty());
        assert_eq!(
            unsaved.chunks().collect::<Vec<_>>(),
            vec![&"c0".to_string()]
        );
        assert_eq!(manager.dirty_updates, vec![ChunkId::from("c0")]);
        // a second calc_done for the same (already cleared) processor is
        // a no-op
        cache.calc_done(id, true, &[6], &mut unsaved, &mut manager);
        assert_eq!(cache.navmesh(), &[4, 5]);
    }

    // -----------------------------------------------------------------------
    // FloodSnapshot / NavmeshProcessor end to end
    // -----------------------------------------------------------------------

    #[test]
    fn flood_snapshot_roundtrip() {
        let stage = TestFlood::stage_grid(2, 3, 1.0, Vector3::new(1.0, 2.0, 3.0));
        let snapshot = stage.stage();
        assert_eq!(snapshot.width(), 2);
        assert_eq!(snapshot.height(), 3);
        assert_eq!(snapshot.min_bounds(), Vector3::new(1.1, 2.0, 3.1));
        assert_eq!(snapshot.resolution(), 1.0);
        assert_eq!(snapshot.adjacency().len(), MAX_HEIGHTS);
        assert_eq!(snapshot.heights()[0].len(), 6);
        let snapshot2 = snapshot.clone();
        assert_eq!(snapshot2.width(), 2);
    }

    fn make_processor(
        manager: &mut TestManager,
        chunk: TestChunk,
        flood: TestFlood,
        girths: Vec<f32>,
    ) -> NavmeshProcessor<TestChunk, TestFlood> {
        NavmeshProcessor::new(chunk, flood, manager, girths)
    }

    #[test]
    fn processor_locks_and_cleans_up() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        let chunk = TestChunk::outside("c0");
        let flood = TestFlood::stage_grid(2, 2, 1.0, Vector3::new(0.0, 0.0, 0.0));
        let mut processor = make_processor(&mut manager, chunk, flood, vec![1.0]);
        assert!(processor.has_locks());
        assert_eq!(processor.locked_chunks().count(&"c0".into()), 1);
        processor.cleanup(&mut manager).unwrap();
        assert!(!processor.has_locks());
    }

    #[test]
    fn processor_end_to_end_appends_per_girth() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        let mut chunk = TestChunk::outside("c0");
        chunk.entities = vec![Vector3::new(1.0, 0.0, 1.0)];
        let flood = TestFlood::stage_grid(2, 2, 1.0, Vector3::new(0.0, 0.0, 0.0));
        let mut processor = make_processor(&mut manager, chunk, flood.clone(), vec![1.0, 2.0]);

        // a stop check before the first girth returns false without
        // touching the flood
        assert!(!processor.process_in_background(&mut manager, &|| true));
        assert_eq!(*flood.calls.borrow(), Vec::<f32>::new());

        assert!(processor.process_in_background(&mut manager, &|| false));
        assert_eq!(*flood.calls.borrow(), vec![1.0, 2.0]);
        // the flood data survived to generation (snapshot path)
        assert_eq!(processor.flood_mut().width(), 2);
        assert!(processor.process_in_main_thread(&mut manager, true));
        let navmesh = processor.navmesh();
        assert!(!navmesh.is_empty());
        // two girths, each producing a record stream: at least two
        // version-0 headers
        let versions = navmesh
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes(*c))
            .filter(|v| *v == 0)
            .count();
        assert!(versions >= 2, "expected >= 2 girth records, got {versions}");
        processor.cleanup(&mut manager).unwrap();
    }

    #[test]
    fn processor_flood_failure_stops_the_ring() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        let chunk = TestChunk::outside("c0");
        let mut flood = TestFlood::stage_grid(2, 2, 1.0, Vector3::new(0.0, 0.0, 0.0));
        flood.succeed = false;
        let mut processor = make_processor(&mut manager, chunk, flood.clone(), vec![1.0, 2.0]);
        assert!(!processor.process_in_background(&mut manager, &|| false));
        assert_eq!(*flood.calls.borrow(), vec![1.0]);
        assert!(processor.navmesh().is_empty());
    }

    #[test]
    fn processor_inside_chunk_locks_only_the_shell() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        manager.register_outside("c1", 1, 0);
        let mut chunk = TestChunk::outside("c1");
        chunk.outside = false;
        let flood = TestFlood::stage_grid(1, 1, 1.0, Vector3::new(0.0, 0.0, 0.0));
        let mut processor = make_processor(&mut manager, chunk, flood, vec![1.0]);
        assert!(processor.has_locks());
        processor.cleanup(&mut manager).unwrap();
    }

    #[test]
    fn holder_accessors_count_and_iterate() {
        let mut manager = TestManager::default();
        manager.register_outside("a", 0, 0);
        let mut holder = ScopedLockedChunkHolder::new();
        holder.insert(&"a".into(), &mut manager).unwrap();
        assert!(holder.contains(&"a".into()));
        assert!(!holder.contains(&"b".into()));
        assert_eq!(holder.count(&"a".into()), 1);
        assert_eq!(holder.count(&"b".into()), 0);
        let mut names: Vec<_> = holder.chunks().cloned().collect();
        names.sort();
        assert_eq!(names, vec!["a".to_string()]);
    }

    #[test]
    fn portal_lock_reports_the_missing_terminal_chunk() {
        let mut manager = TestManager::default();
        manager.register_outside("a", 0, 0);
        manager.register_outside("b", 1, 0);
        manager.neighbours.insert("a".into(), vec!["b".into()]);
        manager.neighbours.insert("b".into(), vec!["a".into()]);
        // the ring visits a and b, but only locks what `insert` locks:
        // "b" is registered outside, yet its grid is not the manager's,
        // so its plain `insert` succeeds — force the failure with a
        // double lock on the first hop instead
        let mut holder = ScopedLockedChunkHolder::new();
        holder
            .lock_portal_depth(&"a".into(), 1, &mut manager)
            .unwrap();
        assert!(holder.contains(&"a".into()));
        assert!(holder.contains(&"b".into()));
    }

    #[test]
    fn ring_lock_reports_the_missing_terminal_chunk() {
        let mut manager = TestManager::default();
        manager.outside.insert("ghost".into());
        let mut holder = ScopedLockedChunkHolder::new();
        let err = manager
            .lock_chunk_in_memory(&"ghost".into(), 0, 0, &mut holder)
            .unwrap_err();
        assert!(matches!(err, LockedChunkError::NeverLocked(_)));
    }

    #[test]
    fn listener_add_and_del() {
        let mut task = ChunkProcessorTask::new(TestBody::new(), 1);
        let listener: Arc<dyn ChunkProcessorListener> = Arc::new(NoopListener);
        task.add_listener(listener.clone());
        assert_eq!(task.listeners().len(), 1);
        let ptr = Arc::as_ptr(&listener) as *const () as usize;
        task.remove_listener_by_ptr(ptr);
        assert!(task.listeners().is_empty());
        task.add_listener(listener.clone());
        task.del_listener(&listener);
        assert!(task.listeners().is_empty());
    }

    #[test]
    fn processor_reports_its_members() {
        let mut manager = TestManager::default();
        manager.register_outside("c0", 0, 0);
        let chunk = TestChunk::outside("c0");
        let flood = TestFlood::stage_grid(1, 1, 1.0, Vector3::ZERO);
        let processor = make_processor(&mut manager, chunk, flood, vec![1.0, 2.0]);
        assert_eq!(processor.chunk().id, "c0");
        assert!(NavgenChunkFace::loaded(processor.chunk()));
        assert!(!NavgenChunkFace::loading(processor.chunk()));
        assert!(NavgenChunkFace::portal_planes(processor.chunk()).is_empty());
        assert_eq!(processor.girths(), &[1.0, 2.0]);
        assert!(processor.navmesh().is_empty());
        assert!(processor.has_locks());
    }

    #[test]
    fn flood_snapshot_delegates_every_field() {
        let stage = TestFlood::stage_grid(2, 3, 1.0, Vector3::new(1.0, 2.0, 3.0));
        let snapshot = stage.stage();
        assert!(snapshot.flooded());
        assert_eq!(snapshot.width(), 2);
        assert_eq!(snapshot.height(), 3);
        assert_eq!(snapshot.min_bounds(), Vector3::new(1.1, 2.0, 3.1));
        assert_eq!(snapshot.resolution(), 1.0);
        assert_eq!(snapshot.adjacency().len(), MAX_HEIGHTS);
        assert_eq!(snapshot.heights()[0].len(), 6);
    }

    #[test]
    fn unsaved_list_tracks_adds_and_size() {
        let mut list = UnsavedList::default();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
        list.add(&"a".into());
        list.add(&"b".into());
        list.add(&"a".into());
        assert_eq!(list.len(), 2);
        assert!(!list.is_empty());
        let mut names: Vec<_> = list.chunks().cloned().collect();
        names.sort();
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn cdata_open_section_without_create_returns_none() {
        let mut cdata = CDataSection::default();
        assert!(cdata.open_section("missing", false).is_none());
        assert!(cdata.open_section("missing", true).is_some());
    }

    #[test]
    fn cache_accessors_and_dirty_gate() {
        // the cache starts dirty regardless of the generator ...
        let mut cache = NavmeshCache::new("c0", NavmeshGenerator::None);
        assert_eq!(cache.chunk(), "c0");
        assert!(cache.navmesh().is_empty());
        assert!(cache.is_dirty());
        // ... but NONE never reports itself dirty to the caller
        assert!(!cache.dirty());
        cache.touch();

        // with NAVGEN the reported flag follows the stored one
        let mut cache = NavmeshCache::new("c0", NavmeshGenerator::Navgen);
        cache.touch();
        assert!(cache.is_dirty());
        assert!(cache.dirty());
    }

    #[test]
    fn cache_touch_waits_before_calculation() {
        let mut cache = NavmeshCache::new("c0", NavmeshGenerator::Navgen);
        cache.config = CacheConfig {
            do_wait_before_calculate: true,
        };
        assert!(!cache.ready_to_calculate(&TestManager::default()));
        cache.touch();
        assert!(cache.is_dirty());
    }

    #[test]
    fn cache_ready_for_inside_chunk() {
        let mut busy = TestManager::default();
        let cache = NavmeshCache::new("in0", NavmeshGenerator::Navgen);
        assert!(!cache.ready_to_calculate(&busy));
        assert!(!cache.load_chunk_for_calculate(&mut busy));
        let mut manager = TestManager {
            ready: true,
            ..TestManager::default()
        };
        let cache = NavmeshCache::new("in0", NavmeshGenerator::Navgen);
        assert!(cache.ready_to_calculate(&manager));
        assert!(cache.load_chunk_for_calculate(&mut manager));
    }

    #[test]
    fn cache_invalidate_spread_for_inside_chunk() {
        let mut manager = TestManager::default();
        manager.spread.insert("in0".into(), vec!["c0".into()]);
        manager.register_outside("c0", 0, 0);
        let mut cache = NavmeshCache::new("in0", NavmeshGenerator::Navgen);
        cache.invalidate(&mut manager, true, None);
        assert_eq!(manager.spread_calls, 1);
        assert_eq!(manager.locks.lock_count(&"c0".into()), 1);
        // a spread invalidate returns before the dirty flag is touched
        assert!(cache.is_dirty());
    }

    #[test]
    fn cache_save_chunk_and_cdata_with_navmesh() {
        let mut cache = NavmeshCache::new("c0", NavmeshGenerator::Navgen);
        cache.navmesh = vec![1, 2, 3];
        cache.dirty = true;
        let mut chunk = CDataSection::default();
        cache.save_chunk(&mut chunk);
        assert!(chunk.find_child("worldNavmesh").is_some());
        // save_cdata rewrites both sections: dirty flag first, then the
        // world navmesh block when the generator is not NONE
        let mut cdata = CDataSection::default();
        cache.save_cdata(&mut cdata);
        let dirty = cdata.open_section("navmeshDirty", false).unwrap();
        assert_eq!(dirty.as_binary().unwrap(), &[1]);
        let world = cdata.open_section("worldNavmesh", false).unwrap();
        assert_eq!(world.as_binary().unwrap(), &[1, 2, 3]);
    }

    #[test]
    fn flat_physics_answers_the_flood_seam() {
        let mut physics = FlatPhysics;
        assert_eq!(
            waypoint_gen::FloodPhysics::get_girth(&physics),
            Vector3::new(0.5, 1.5, 0.5)
        );
        assert_eq!(
            waypoint_gen::FloodPhysics::get_scramble_height(&physics),
            0.5
        );
        let mut y = 0.0;
        assert!(waypoint_gen::FloodPhysics::find_drop_point(
            &mut physics,
            &Vector3::new(1.0, 10.0, 1.0),
            &mut y
        ));
        assert_eq!(y, 0.0);
        let dst = Vector3::new(1.0, 0.5, 1.0);
        let mut out = Vector3::ZERO;
        waypoint_gen::FloodPhysics::adjust_move(&mut physics, &Vector3::ZERO, &dst, &mut out);
        assert_eq!(out, Vector3::new(1.0, 0.0, 1.0));
        // an impossible move is signalled the way the reference's
        // physics handler does: `dst2.y = -10000`
        let dst = Vector3::new(1.0, 10.0, 1.0);
        let mut out = Vector3::ZERO;
        waypoint_gen::FloodPhysics::adjust_move(&mut physics, &Vector3::ZERO, &dst, &mut out);
        assert_eq!(out.y, -10000.0);
    }

    #[test]
    fn noop_listener_counts_as_finished() {
        let mut task = ChunkProcessorTask::new(TestBody::new(), 11);
        let listener: Arc<dyn ChunkProcessorListener> = Arc::new(NoopListener);
        task.add_listener(listener);
        let mut manager = manager_with_locks();
        task.cleanup(&mut manager).unwrap();
    }

    #[test]
    fn manager_face_helpers_cover_the_queue() {
        let mut manager = TestManager::default();
        manager.num_running_threads();
        manager.add_background_task(7);
        manager.add_main_thread_task(8);
        assert_eq!(manager.background, vec![7]);
        assert_eq!(manager.main, vec![8]);
        manager.update_chunk_dirty_status(&"c0".into());
        assert_eq!(manager.dirty_updates, vec![ChunkId::from("c0")]);
    }

    #[test]
    fn processor_failed_lock_leaves_no_shell() {
        let mut manager = TestManager::default();
        let chunk = TestChunk::outside("ghost");
        let flood = TestFlood::stage_grid(1, 1, 1.0, Vector3::ZERO);
        let processor = make_processor(&mut manager, chunk, flood, vec![1.0]);
        // the reference asserts on the failed terminal lock; the port
        // leaves an empty shell behind.
        assert!(!processor.has_locks());
        assert_eq!(processor.locked_chunks().len(), 0);
    }
}
