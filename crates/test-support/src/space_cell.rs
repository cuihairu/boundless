//! BW-25-01-03: Space/Cell ownership and the area of interest.
//!
//! The port answers what the reference's `cell_viewer_server.cpp` answers:
//! which space and cell holds an entity, who is in a viewer's area of
//! interest, and — when an entity migrates — where it was and where it went.
//! Ownership stays queryable across a migration, so before and after are both
//! on record.

use std::collections::{BTreeMap, BTreeSet};

/// One migration step: where the entity came from and where it went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationStep {
    /// The (space, cell) the entity came from.
    pub from: (u64, u64),
    /// The (space, cell) the entity moved to.
    pub to: (u64, u64),
}

/// An entity's cell assignment and its migration history, together with the
/// area-of-interest membership per cell.
#[derive(Debug, Default)]
pub struct SpaceCellRegistry {
    /// Entity id to its (space, cell) assignment.
    assignments: BTreeMap<u64, (u64, u64)>,
    /// Entity id to its migration chain, oldest first.
    migrations: BTreeMap<u64, Vec<MigrationStep>>,
    /// (space, cell) to viewer to the set of entities it sees.
    aoi: BTreeMap<(u64, u64), BTreeMap<u64, BTreeSet<u64>>>,
}

impl SpaceCellRegistry {
    /// A registry with no assignments, migrations or AOI.
    pub fn new() -> Self {
        Self::default()
    }

    /// Assign an entity to a space and cell.
    pub fn assign(&mut self, entity: u64, space: u64, cell: u64) {
        self.assignments.insert(entity, (space, cell));
    }

    /// The space and cell an entity is in, when it is assigned.
    pub fn owner(&self, entity: u64) -> Option<(u64, u64)> {
        self.assignments.get(&entity).copied()
    }

    /// Migrate an entity to another cell. Returns where it came from, or
    /// `None` when the entity was not assigned (so nothing moved).
    pub fn migrate(&mut self, entity: u64, to_space: u64, to_cell: u64) -> Option<(u64, u64)> {
        let from = self.owner(entity)?;
        let to = (to_space, to_cell);
        self.assignments.insert(entity, to);
        self.migrations
            .entry(entity)
            .or_default()
            .push(MigrationStep { from, to });
        Some(from)
    }

    /// The migration chain of an entity, oldest first.
    pub fn migration_history(&self, entity: u64) -> Vec<MigrationStep> {
        self.migrations.get(&entity).cloned().unwrap_or_default()
    }

    /// Record that `viewer`, in a space and cell, sees each entity in `seen`.
    pub fn observe(&mut self, space: u64, cell: u64, viewer: u64, seen: &[u64]) {
        self.aoi
            .entry((space, cell))
            .or_default()
            .entry(viewer)
            .or_default()
            .extend(seen.iter().copied());
    }

    /// The entities `viewer` sees in a space and cell, ascending.
    pub fn seen_by(&self, space: u64, cell: u64, viewer: u64) -> Vec<u64> {
        self.aoi
            .get(&(space, cell))
            .and_then(|cell_aoi| cell_aoi.get(&viewer))
            .map(|seen| seen.iter().copied().collect())
            .unwrap_or_default()
    }

    /// The entities assigned to a space and cell, ascending.
    pub fn cell_entities(&self, space: u64, cell: u64) -> Vec<u64> {
        let mut entities: Vec<u64> = self
            .assignments
            .iter()
            .filter(|(_, (s, c))| *s == space && *c == cell)
            .map(|(entity, _)| *entity)
            .collect();
        entities.sort_unstable();
        entities
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_is_queryable_before_and_after_migration() {
        let mut registry = SpaceCellRegistry::new();
        registry.assign(7, 1, 10);

        // Before the migration: the entity is in space 1, cell 10.
        assert_eq!(registry.owner(7), Some((1, 10)));

        // Migrate to space 2, cell 20: where it came from is returned, and
        // the owner is the new cell.
        let from = registry.migrate(7, 2, 20);
        assert_eq!(from, Some((1, 10)));
        assert_eq!(registry.owner(7), Some((2, 20)));

        // Both sides are on record: the migration chain holds before and
        // after.
        assert_eq!(
            registry.migration_history(7),
            vec![MigrationStep {
                from: (1, 10),
                to: (2, 20)
            }]
        );
        assert_eq!(registry.cell_entities(2, 20), vec![7]);
        assert_eq!(registry.cell_entities(1, 10), Vec::<u64>::new());
    }

    #[test]
    fn an_entity_migrating_twice_keeps_its_whole_chain() {
        let mut registry = SpaceCellRegistry::new();
        registry.assign(7, 1, 10);
        registry.migrate(7, 1, 11);
        registry.migrate(7, 2, 20);

        assert_eq!(registry.owner(7), Some((2, 20)));
        assert_eq!(
            registry.migration_history(7),
            vec![
                MigrationStep {
                    from: (1, 10),
                    to: (1, 11)
                },
                MigrationStep {
                    from: (1, 11),
                    to: (2, 20)
                }
            ]
        );
    }

    #[test]
    fn migrating_an_unassigned_entity_moves_nothing() {
        let mut registry = SpaceCellRegistry::new();
        assert_eq!(registry.owner(7), None);
        assert_eq!(registry.migrate(7, 2, 20), None);
        assert_eq!(registry.owner(7), None);
        assert!(registry.migration_history(7).is_empty());
    }

    #[test]
    fn aoi_membership_is_collected_per_cell() {
        let mut registry = SpaceCellRegistry::new();
        registry.observe(1, 10, 7, &[8, 9]);
        registry.observe(1, 10, 8, &[7]);
        // Another cell has its own AOI; the two never mix.
        registry.observe(1, 11, 7, &[12]);

        assert_eq!(registry.seen_by(1, 10, 7), vec![8, 9]);
        assert_eq!(registry.seen_by(1, 10, 8), vec![7]);
        assert_eq!(registry.seen_by(1, 11, 7), vec![12]);
        assert_eq!(registry.seen_by(1, 10, 99), Vec::<u64>::new());
    }
}
