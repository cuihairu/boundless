//! BW-08-01-01: create and destroy a Space.
//!
//! The reference `server/cellapp/spaces.cpp` owns the live spaces of one
//! CellApp: `Spaces::create` (`spaces.cpp:36`) inserts a fresh [`Space`], and
//! `Spaces::deleteOldSpaces` (`:86`) reclaims exactly the spaces that have no
//! cell left and are fully unloaded — no entities and a fully unloaded
//! physical space. `Space::~Space` (`space.cpp:95`) destroys the ghost
//! entities it still holds (real entities belong to the cell), cancels the
//! shutdown timer, deletes the cell info tree and clears the physical space;
//! `Space::reuse` (`:139`) puts a space into a state fit for re-creation.

use std::collections::BTreeMap;

/// `SpaceID` (`space.hpp`).
pub type SpaceId = u32;

/// `NULL_SPACE_ID`.
pub const NULL_SPACE_ID: SpaceId = 0;

/// `EntityID`.
pub type EntityId = u32;

/// The failure modes of space management.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceError {
    /// The space already exists (`MF_ASSERT(this->find(id) == NULL)`,
    /// `spaces.cpp:36`).
    Exists,
    /// The space was not found.
    NotFound,
}

/// The physical space backing a space's chunk load state (`PhysicalSpacePtr`,
/// `space.hpp:58`). The reference picks the delegate flavour when a delegated
/// game is running and the chunk flavour otherwise (`space.cpp:78`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhysicalSpace {
    /// `PhysicalChunkSpace`: tracks how many chunks this space has loaded.
    ChunkSpace {
        /// The number of chunks currently held.
        loaded: usize,
    },
    /// `PhysicalDelegateSpace`: the delegated game supplies the space, and the
    /// reference always reports it fully unloaded.
    Delegate,
}

impl PhysicalSpace {
    /// `PhysicalSpace::isFullyUnloaded`: no chunks are held.
    pub fn fully_unloaded(&self) -> bool {
        match self {
            PhysicalSpace::ChunkSpace { loaded } => *loaded == 0,
            PhysicalSpace::Delegate => true,
        }
    }

    /// `PhysicalSpace::clear` (`space.cpp:122`, the destructor's last step).
    pub fn clear(&mut self) {
        if let PhysicalSpace::ChunkSpace { loaded } = self {
            *loaded = 0;
        }
    }

    /// `PhysicalSpace::reuse`.
    pub fn reuse(&mut self) {
        self.clear();
    }

    /// Whether this is the delegated flavour.
    pub fn is_delegated(&self) -> bool {
        matches!(self, PhysicalSpace::Delegate)
    }
}

/// One cell of a space, as the space sees it: an address on a CellApp
/// (`Cell`, `cell.hpp`). The cell that owns the real entities is the space's
/// own `pCell`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRef {
    /// The CellApp the cell runs on.
    pub app: crate::cellappmgr::Addr,
}

/// An entity a space holds, with just the state its lifecycle needs
/// (`EntityPtr` in `SpaceEntities`, `space.hpp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceEntity {
    /// The entity's id.
    pub id: EntityId,
    /// Whether this cell holds the real for the entity.
    pub is_real: bool,
    /// Whether the entity has already been destroyed.
    pub is_destroyed: bool,
}

/// What destroying a space released, mirroring the steps of
/// `Space::~Space` (`space.cpp:95`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpaceDestroyReport {
    /// The ghost entities the destructor destroyed.
    pub ghosts_destroyed: Vec<EntityId>,
    /// The real entities left behind, each of which the destructor warns
    /// about (`space.cpp:108`).
    pub real_entities_undestroyed: Vec<EntityId>,
    /// Whether a cell info tree had to be deleted.
    pub cell_info_tree_deleted: bool,
    /// Whether the physical space held chunks that were cleared.
    pub physical_space_cleared: bool,
    /// Whether a shutdown timer was still pending and got cancelled.
    pub shutting_down_timer_cancelled: bool,
}

/// One space of one CellApp (`Space`, `space.cpp:62`).
#[derive(Debug, Clone)]
pub struct Space {
    id: SpaceId,
    /// `pCell_`: the cell controlling the real entities, `None` once the cell
    /// is gone.
    cell: Option<CellRef>,
    /// `pPhysicalSpace_`.
    physical_space: PhysicalSpace,
    /// `entities_`, by id.
    entities: BTreeMap<EntityId, SpaceEntity>,
    /// `pCellInfoTree_`: whether the partitioning tree is still held.
    cell_info_tree: bool,
    /// `shuttingDownTimerHandle_`: whether a shutdown timer is pending.
    shutting_down_timer: bool,
}

impl Space {
    /// A fresh space (`Space::Space`, `space.cpp:62`): no cell, no entities,
    /// no tree, and a physical space of the caller's flavour.
    pub fn new(id: SpaceId, delegated: bool) -> Self {
        Space {
            id,
            cell: None,
            physical_space: if delegated {
                PhysicalSpace::Delegate
            } else {
                PhysicalSpace::ChunkSpace { loaded: 0 }
            },
            entities: BTreeMap::new(),
            cell_info_tree: false,
            shutting_down_timer: false,
        }
    }

    /// The space's id.
    pub fn id(&self) -> SpaceId {
        self.id
    }

    /// `pCell()` (`space.hpp`).
    pub fn p_cell(&self) -> Option<&CellRef> {
        self.cell.as_ref()
    }

    /// `pCell(Cell*)` (`space.cpp:164`): attach or detach the cell that
    /// controls this space's real entities.
    pub fn set_cell(&mut self, cell: Option<CellRef>) {
        self.cell = cell;
    }

    /// The physical space.
    pub fn physical_space(&self) -> &PhysicalSpace {
        &self.physical_space
    }

    /// Mutable access to the physical space, for loading and unloading
    /// chunks.
    pub fn physical_space_mut(&mut self) -> &mut PhysicalSpace {
        &mut self.physical_space
    }

    /// `Space::addEntity` (`space.cpp:301`).
    pub fn add_entity(&mut self, entity: SpaceEntity) {
        self.entities.insert(entity.id, entity);
    }

    /// `Space::removeEntity` (`space.cpp:315`).
    pub fn remove_entity(&mut self, id: EntityId) -> Option<SpaceEntity> {
        self.entities.remove(&id)
    }

    /// The entities held, by id.
    pub fn entities(&self) -> impl Iterator<Item = &SpaceEntity> {
        self.entities.values()
    }

    /// The number of entities held.
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// Whether the cell info tree is held (`pCellInfoTree_ != NULL`).
    pub fn has_cell_info_tree(&self) -> bool {
        self.cell_info_tree
    }

    /// Take up the partitioning tree, as building a cell does
    /// (`pCellInfoTree_ = new CellInfo...`).
    pub fn set_cell_info_tree(&mut self, present: bool) {
        self.cell_info_tree = present;
    }

    /// Whether a shutdown timer is pending.
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down_timer
    }

    /// Request the shutdown timer (`Space::requestShutDown`,
    /// `space.cpp:1125`).
    pub fn request_shut_down(&mut self) {
        self.shutting_down_timer = true;
    }

    /// `Space::isFullyUnloaded` (`space.cpp:1225`): no entities remain and the
    /// physical space holds nothing.
    pub fn is_fully_unloaded(&self) -> bool {
        self.entities.is_empty() && self.physical_space.fully_unloaded()
    }

    /// `Space::reuse` (`space.cpp:139`): reset the space into a state fit for
    /// re-creation.
    pub fn reuse(&mut self) {
        self.cell_info_tree = false;
        self.physical_space.reuse();
    }

    /// `Space::~Space` (`space.cpp:95`): destroy the ghosts, warn about the
    /// reals, cancel the shutdown timer, delete the tree and clear the
    /// physical space.
    pub fn destroy_all(&mut self) -> SpaceDestroyReport {
        let mut report = SpaceDestroyReport::default();

        // Destroy any ghosts we have; real entities should be taken care of
        // by ~Cell (`space.cpp:97`).
        let ids: Vec<EntityId> = self.entities.keys().copied().collect();
        for id in ids {
            let entity = self
                .entities
                .get_mut(&id)
                .expect("key just collected from the map");
            if !entity.is_real && !entity.is_destroyed {
                entity.is_destroyed = true;
                self.entities.remove(&id);
                report.ghosts_destroyed.push(id);
            } else if entity.is_real && !entity.is_destroyed {
                report.real_entities_undestroyed.push(id);
            }
        }

        report.shutting_down_timer_cancelled = self.shutting_down_timer;
        self.shutting_down_timer = false;

        report.cell_info_tree_deleted = self.cell_info_tree;
        self.cell_info_tree = false;

        report.physical_space_cleared = !self.physical_space.fully_unloaded();
        self.physical_space.clear();

        report
    }
}

/// The live spaces of one CellApp (`Spaces`, `spaces.hpp`).
#[derive(Debug, Clone, Default)]
pub struct Spaces {
    container: BTreeMap<SpaceId, Space>,
}

impl Spaces {
    /// An empty set of spaces.
    pub fn new() -> Self {
        Spaces::default()
    }

    /// `Spaces::find` (`spaces.cpp:27`).
    pub fn find(&self, id: SpaceId) -> Option<&Space> {
        self.container.get(&id)
    }

    /// `Spaces::find`, mutably.
    pub fn find_mut(&mut self, id: SpaceId) -> Option<&mut Space> {
        self.container.get_mut(&id)
    }

    /// `Spaces::create` (`spaces.cpp:36`): a fresh space, rejected when the
    /// id is already live.
    pub fn create(&mut self, id: SpaceId, delegated: bool) -> Result<&mut Space, SpaceError> {
        if self.container.contains_key(&id) {
            return Err(SpaceError::Exists);
        }
        self.container.insert(id, Space::new(id, delegated));
        Ok(self.container.get_mut(&id).expect("just inserted"))
    }

    /// The number of live spaces.
    pub fn len(&self) -> usize {
        self.container.len()
    }

    /// Whether no space is live.
    pub fn is_empty(&self) -> bool {
        self.container.is_empty()
    }

    /// The live spaces, by id.
    pub fn iter(&self) -> impl Iterator<Item = (&SpaceId, &Space)> {
        self.container.iter()
    }

    /// `Spaces::deleteOldSpaces` (`spaces.cpp:86`): reclaim the spaces that
    /// have no cell and are fully unloaded, destroying each on the way out.
    /// Returns the reclaimed ids.
    pub fn delete_old_spaces(&mut self) -> Vec<SpaceId> {
        let stale: Vec<SpaceId> = self
            .container
            .iter()
            .filter(|(_, space)| space.p_cell().is_none() && space.is_fully_unloaded())
            .map(|(id, _)| *id)
            .collect();

        for id in &stale {
            if let Some(mut space) = self.container.remove(id) {
                space.destroy_all();
            }
        }

        stale
    }

    /// `Spaces::~Spaces` (`spaces.cpp:13`): destroy every space still held.
    pub fn destroy_all(&mut self) -> Vec<(SpaceId, SpaceDestroyReport)> {
        let mut reports = Vec::new();
        for (id, space) in &mut self.container {
            reports.push((*id, space.destroy_all()));
        }
        self.container.clear();
        reports
    }
}

impl Drop for Spaces {
    fn drop(&mut self) {
        self.destroy_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ghost(id: EntityId) -> SpaceEntity {
        SpaceEntity {
            id,
            is_real: false,
            is_destroyed: false,
        }
    }

    fn real(id: EntityId) -> SpaceEntity {
        SpaceEntity {
            id,
            is_real: true,
            is_destroyed: false,
        }
    }

    /// A space registers on creation and is found afterwards.
    #[test]
    fn a_created_space_is_found_by_its_id() {
        let mut spaces = Spaces::new();
        let space = spaces.create(7, false).expect("fresh space");
        assert_eq!(space.id(), 7);
        assert!(spaces.find(7).is_some());
        assert_eq!(spaces.len(), 1);
    }

    /// Creating the same space twice is rejected.
    #[test]
    fn creating_a_space_twice_is_rejected() {
        let mut spaces = Spaces::new();
        spaces.create(7, false).expect("fresh space");
        assert_eq!(spaces.create(7, false).unwrap_err(), SpaceError::Exists);
        assert_eq!(spaces.len(), 1, "the second create added nothing");
    }

    /// An empty space with no cell is reclaimed; one with a cell, entities or
    /// loaded chunks is not.
    #[test]
    fn only_cellless_fully_unloaded_spaces_are_reclaimed() {
        let mut spaces = Spaces::new();
        spaces.create(1, false).expect("empty, cellless");

        spaces.create(2, false).expect("has a cell");
        spaces
            .find_mut(2)
            .unwrap()
            .set_cell(Some(CellRef { app: (0, 0) }));

        spaces.create(3, false).expect("has an entity");
        spaces.find_mut(3).unwrap().add_entity(ghost(11));

        spaces.create(4, false).expect("has loaded chunks");
        spaces.find_mut(4).unwrap().physical_space_mut().clear();
        if let PhysicalSpace::ChunkSpace { loaded } =
            spaces.find_mut(4).unwrap().physical_space_mut()
        {
            *loaded = 2;
        }

        let reclaimed = spaces.delete_old_spaces();
        assert_eq!(reclaimed, vec![1]);
        assert!(spaces.find(1).is_none());
        for id in [2, 3, 4] {
            assert!(spaces.find(id).is_some(), "space {id} survives");
        }
    }

    /// Destroying a space destroys its ghosts and reports the reals it had to
    /// leave behind.
    #[test]
    fn destroying_a_space_destroys_its_ghosts_and_reports_the_reals() {
        let mut space = Space::new(9, false);
        space.add_entity(ghost(11));
        space.add_entity(ghost(12));
        space.add_entity(real(13));
        space.set_cell_info_tree(true);
        space.request_shut_down();
        if let PhysicalSpace::ChunkSpace { loaded } = space.physical_space_mut() {
            *loaded = 1;
        }

        let report = space.destroy_all();
        assert_eq!(report.ghosts_destroyed, vec![11, 12]);
        assert_eq!(report.real_entities_undestroyed, vec![13]);
        assert!(report.cell_info_tree_deleted);
        assert!(report.shutting_down_timer_cancelled);
        assert!(report.physical_space_cleared);
        assert!(!space.has_cell_info_tree());
        assert!(!space.is_shutting_down());
        assert!(space.physical_space().fully_unloaded());
    }

    /// A delegated space always reports itself fully unloaded.
    #[test]
    fn a_delegated_space_is_always_fully_unloaded() {
        let mut space = Space::new(5, true);
        assert!(space.physical_space().is_delegated());
        assert!(space.physical_space().fully_unloaded());
        space.physical_space_mut().clear();
        assert!(space.physical_space().fully_unloaded());
    }

    /// Reuse resets the derived state but keeps the space's identity.
    #[test]
    fn reuse_resets_the_derived_state() {
        let mut space = Space::new(6, false);
        space.set_cell_info_tree(true);
        if let PhysicalSpace::ChunkSpace { loaded } = space.physical_space_mut() {
            *loaded = 3;
        }

        space.reuse();

        assert_eq!(space.id(), 6);
        assert!(!space.has_cell_info_tree());
        assert!(space.physical_space().fully_unloaded());
    }

    /// Dropping the spaces destroys every space it still holds.
    #[test]
    fn dropping_the_spaces_destroys_them_all() {
        let mut spaces = Spaces::new();
        spaces.create(1, false).expect("space one");
        spaces.find_mut(1).unwrap().add_entity(ghost(11));
        spaces.create(2, false).expect("space two");

        let reports = spaces.destroy_all();
        assert_eq!(
            reports,
            vec![
                (
                    1,
                    SpaceDestroyReport {
                        ghosts_destroyed: vec![11],
                        ..Default::default()
                    }
                ),
                (2, SpaceDestroyReport::default()),
            ]
        );
        assert!(spaces.is_empty());
    }

    /// Removing an entity unblocks the space's reclamation.
    #[test]
    fn removing_the_last_entity_unblocks_reclamation() {
        let mut spaces = Spaces::new();
        spaces.create(8, false).expect("space");
        spaces.find_mut(8).unwrap().add_entity(ghost(21));
        assert!(spaces.delete_old_spaces().is_empty());

        spaces.find_mut(8).unwrap().remove_entity(21);
        assert_eq!(spaces.delete_old_spaces(), vec![8]);
    }
}
