//! BW-07-04-04: periodic archiving.
//!
//! A BaseApp persists a slice of its entities to the database each tick, so the
//! whole set is written once per `archivePeriodInTicks` (`Archiver::tick`,
//! `archiver.cpp:69`). Archiving is the second level of fault tolerance after
//! backup (`baseapp.cpp:2776`): the first level keeps a copy on another
//! BaseApp, the second keeps the entity's latest state in the database so a
//! restart can recover it.
//!
//! Only entities that have previously been written to the database
//! (`hasWrittenToDB`, `base.hpp:85` — a non-zero `databaseID_`) join the cycle,
//! and the order is randomised so a block of one entity type does not hit the
//! database at once (`restartArchiveCycle`, `archiver.cpp:144`). When a BaseApp
//! dies its restored entities are persisted promptly: the dead app is recorded
//! (`handleBaseAppDeath`, `archiver.cpp:129`) and once the archive cycle
//! restarts BaseAppMgr is told the archive is complete
//! (`informOfArchiveComplete`). `Base::autoArchive` (`base.cpp:1899`) applies
//! the `shouldAutoArchive_` policy — `YES` archives every cycle, `NEXT_ONLY`
//! once, `NO` never.
//!
//! This module models the cycle, the death handshake and the persisted store:
//! an archived entity is written to the store keyed by its database id, and
//! after a restart [`restore_archived`] reads the store back to rebuild the
//! entity. The archive payload is the same `writeBackupData` stream the backup
//! path uses.

use crate::base_backup::{BackupError, restore_from_stream, write_backup_data};
use crate::baseappmgr::Addr;
use crate::entity_creator::{BaseEntity, DatabaseId, EntityId, EntityType};
use foundation::stream::BinaryStream;
use std::collections::BTreeMap;

/// `AutoBackupAndArchive::Policy` (`base.hpp:389`): whether a base is archived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoArchivePolicy {
    /// Archive every cycle (`YES`, the default at `base.cpp:639`).
    Always,
    /// Archive once, then become `Never` (`NEXT_ONLY`).
    NextOnly,
    /// Never archive (`NO`).
    Never,
}

/// What one [`Archiver::tick`] did: the entities archived and the app whose
/// archive just completed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TickOutcome {
    /// The entity ids written to the store this tick, in archive order.
    pub archived: Vec<EntityId>,
    /// The dead app to tell `BaseAppMgr::informOfArchiveComplete` about, if the
    /// cycle just restarted and a death was pending.
    pub inform_archive_complete: Option<Addr>,
}

/// The periodic archiver (`Archiver`, `archiver.hpp`): walks the persisted
/// entities a slice at a time so the whole set is written once per period.
#[derive(Debug, Clone)]
pub struct Archiver {
    /// `archiveIndex_`, the tick within the current cycle.
    archive_index: usize,
    /// `archivePeriodInTicks` (`BaseAppConfig`, `archiver.cpp:72`).
    period: usize,
    /// The seed advancing each cycle, standing in for the `std::random_device`
    /// the reference shuffles with (`archiver.cpp:162`).
    seed: u64,
    /// `basesToArchive_`, the persisted entity ids in archive order.
    bases_to_archive: Vec<EntityId>,
    /// `deadBaseAppAddr_`, the dead app awaiting `informOfArchiveComplete`.
    dead_base_app_addr: Option<Addr>,
    /// `pSecondaryDB != NULL` (`archiver.cpp:134`): a death restarts the cycle.
    secondary_db: bool,
    /// `shouldAutoArchive_` per entity, defaulting to `Always`.
    policies: BTreeMap<EntityId, AutoArchivePolicy>,
}

impl Archiver {
    /// A new archiver for the given archive period. The initial index mirrors
    /// `INT_MAX` (`archiver.cpp:25`) so the first tick starts a cycle.
    pub fn new(archive_period_in_ticks: usize) -> Self {
        Archiver {
            archive_index: usize::MAX,
            period: archive_period_in_ticks,
            seed: 0x9e377e55,
            bases_to_archive: Vec::new(),
            dead_base_app_addr: None,
            secondary_db: false,
            policies: BTreeMap::new(),
        }
    }

    /// Whether a secondary database is in use, so a BaseApp death restarts the
    /// archive cycle at once (`archiver.cpp:134`).
    pub fn with_secondary_db(mut self, secondary_db: bool) -> Self {
        self.secondary_db = secondary_db;
        self
    }

    /// `Archiver::isEnabled` (`archiver.cpp:173`): a positive period enables
    /// archiving.
    pub fn is_enabled(&self) -> bool {
        self.period > 0
    }

    /// The current archive order, the persisted entities of the cycle.
    pub fn bases_to_archive(&self) -> &[EntityId] {
        &self.bases_to_archive
    }

    /// `Base::shouldAutoArchive` (`base.hpp:278`): set an entity's policy.
    pub fn set_auto_archive(&mut self, entity_id: EntityId, policy: AutoArchivePolicy) {
        self.policies.insert(entity_id, policy);
    }

    /// The current policy for an entity, `Always` by default.
    pub fn auto_archive_policy(&self, entity_id: EntityId) -> AutoArchivePolicy {
        self.policies
            .get(&entity_id)
            .copied()
            .unwrap_or(AutoArchivePolicy::Always)
    }

    /// `Archiver::tick` (`archiver.cpp:69`): archive this tick's slice of the
    /// persisted entities to the store.
    pub fn tick(
        &mut self,
        bases: &BTreeMap<EntityId, BaseEntity>,
        store: &mut ArchiveStore,
    ) -> TickOutcome {
        let mut outcome = TickOutcome::default();
        if !self.is_enabled() {
            return outcome;
        }

        if self.archive_index >= self.period {
            if let Some(dead) = self.dead_base_app_addr.take() {
                outcome.inform_archive_complete = Some(dead);
            }
            self.restart_archive_cycle(bases);
        }

        let size = self.bases_to_archive.len();
        let start = size * self.archive_index / self.period;
        self.archive_index += 1;
        let end = size * self.archive_index / self.period;

        let slice: Vec<EntityId> = self.bases_to_archive[start..end].to_vec();
        for id in slice.iter().copied() {
            let Some(base) = bases.get(&id) else {
                continue;
            };
            if base.db_id == 0 {
                continue;
            }
            if !self.auto_archive(id) {
                continue;
            }
            store.write(base.db_id, encode(base));
            outcome.archived.push(id);
        }
        outcome
    }

    /// `Archiver::handleBaseAppDeath` (`archiver.cpp:129`): record the dead
    /// app; with a secondary database the cycle restarts at once so the
    /// restored entities are persisted promptly. Returns whether the cycle
    /// restarted.
    pub fn handle_base_app_death(
        &mut self,
        dead_addr: Addr,
        bases: &BTreeMap<EntityId, BaseEntity>,
    ) -> bool {
        self.dead_base_app_addr = Some(dead_addr);
        if self.secondary_db {
            self.restart_archive_cycle(bases);
            true
        } else {
            false
        }
    }

    /// `Archiver::restartArchiveCycle` (`archiver.cpp:144`): begin a fresh
    /// cycle over the persisted entities, in a shuffled order.
    fn restart_archive_cycle(&mut self, bases: &BTreeMap<EntityId, BaseEntity>) {
        self.archive_index = 0;
        self.bases_to_archive.clear();
        for (id, base) in bases.iter() {
            if base.db_id != 0 {
                self.bases_to_archive.push(*id);
            }
        }
        shuffle(&mut self.bases_to_archive, &mut self.seed);
    }

    /// `Base::autoArchive` (`base.cpp:1899`): whether to archive the entity,
    /// applying and advancing the `NEXT_ONLY` policy.
    fn auto_archive(&mut self, entity_id: EntityId) -> bool {
        match self.auto_archive_policy(entity_id) {
            AutoArchivePolicy::Never => false,
            AutoArchivePolicy::Always => true,
            AutoArchivePolicy::NextOnly => {
                self.policies.insert(entity_id, AutoArchivePolicy::Never);
                true
            }
        }
    }
}

/// The persisted entity state (`SqliteDatabase` in the reference): the archive
/// bytes keyed by database id, latest write wins.
#[derive(Debug, Clone, Default)]
pub struct ArchiveStore {
    /// The persisted bytes by database id.
    entries: BTreeMap<DatabaseId, Vec<u8>>,
}

impl ArchiveStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write (or replace) the persisted state for a database id.
    pub fn write(&mut self, db_id: DatabaseId, data: Vec<u8>) {
        self.entries.insert(db_id, data);
    }

    /// The persisted bytes for a database id, or `None` if never archived.
    pub fn get(&self, db_id: DatabaseId) -> Option<&[u8]> {
        self.entries.get(&db_id).map(|data| data.as_slice())
    }

    /// Whether anything is persisted.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many entities are persisted.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The persisted bytes by database id.
    pub fn entries(&self) -> &BTreeMap<DatabaseId, Vec<u8>> {
        &self.entries
    }
}

/// Read a persisted entity back after a restart
/// (`createBaseFromStream`, `baseapp.cpp:2589`): `None` if nothing was
/// persisted for the database id, otherwise the rebuild outcome. `lookup`
/// resolves a type name to its `EntityType`.
pub fn restore_archived(
    store: &ArchiveStore,
    db_id: DatabaseId,
    lookup: &impl Fn(&str) -> Option<EntityType>,
) -> Option<Result<BaseEntity, BackupError>> {
    let data = store.get(db_id)?;
    Some(restore_bytes(data, lookup))
}

/// Rebuild an entity from persisted archive bytes: one pass, the header
/// read once and the body off the same stream (`restore_from_stream`).
fn restore_bytes(
    data: &[u8],
    lookup: &impl Fn(&str) -> Option<EntityType>,
) -> Result<BaseEntity, BackupError> {
    restore_from_stream(&mut BinaryStream::from_slice(data), lookup)
}

/// The archive payload: the same `writeBackupData` stream as the backup path.
/// The written buffer moves out of the stream instead of being copied out.
fn encode(base: &BaseEntity) -> Vec<u8> {
    let mut stream = BinaryStream::new();
    write_backup_data(base, &mut stream);
    stream.into_bytes()
}

/// A deterministic Fisher–Yates shuffle from a linear congruential generator
/// standing in for the reference's `std::shuffle` with `std::random_device`.
fn shuffle(items: &mut [EntityId], seed: &mut u64) {
    for i in (1..items.len()).rev() {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let j = (*seed as usize) % (i + 1);
        items.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_creator::PropValue;
    use std::collections::HashMap;

    fn account_type() -> EntityType {
        EntityType {
            id: 1,
            name: "Account".to_string(),
            can_be_on_base: true,
            is_service: false,
            is_proxy: false,
            can_be_on_cell: true,
            properties: vec![],
            methods: vec![],
        }
    }

    fn lookup_of(ty: EntityType) -> impl Fn(&str) -> Option<EntityType> {
        let name = ty.name.clone();
        move |queried: &str| (queried == name).then(|| ty.clone())
    }

    fn persisted_base(id: EntityId, db_id: DatabaseId) -> BaseEntity {
        BaseEntity {
            id,
            type_id: 1,
            type_name: "Account".to_string(),
            db_id,
            is_proxy: false,
            destroyed: false,
            in_destroy: false,
            base_props: HashMap::new(),
            cell_props: HashMap::new(),
            is_create_cell_pending: false,
            is_get_cell_pending: false,
            cell_entity_addr: None,
            cell_space_id: 0,
            callbacks: Vec::new(),
        }
    }

    fn bases_of(entities: &[BaseEntity]) -> BTreeMap<EntityId, BaseEntity> {
        entities
            .iter()
            .map(|base| (base.id, base.clone()))
            .collect()
    }

    /// Archiving is disabled when the period is zero: the tick archives nothing.
    #[test]
    fn archiving_is_disabled_with_no_period() {
        let mut archiver = Archiver::new(0);
        let bases = bases_of(&[persisted_base(1, 7)]);
        let mut store = ArchiveStore::new();

        assert!(!archiver.is_enabled());
        let outcome = archiver.tick(&bases, &mut store);
        assert!(outcome.archived.is_empty());
        assert!(store.is_empty());
    }

    /// Over a full period every persisted entity is archived exactly once, a
    /// slice per tick.
    #[test]
    fn a_cycle_spreads_entities_over_the_period_ticks() {
        let mut archiver = Archiver::new(4);
        let bases = bases_of(&[
            persisted_base(1, 11),
            persisted_base(2, 22),
            persisted_base(3, 33),
            persisted_base(4, 44),
        ]);
        let mut store = ArchiveStore::new();

        let mut all = Vec::new();
        for _ in 0..4 {
            let outcome = archiver.tick(&bases, &mut store);
            assert_eq!(outcome.archived.len(), 1, "one entity per tick");
            all.extend(outcome.archived);
        }
        all.sort();
        assert_eq!(all, vec![1, 2, 3, 4]);
        assert_eq!(store.len(), 4);
    }

    /// Only entities that have been written to the database join the cycle.
    #[test]
    fn only_persisted_entities_are_archived() {
        let mut archiver = Archiver::new(2);
        let bases = bases_of(&[
            persisted_base(1, 11),
            persisted_base(2, 0), // never written to the database
            persisted_base(3, 33),
        ]);
        let mut store = ArchiveStore::new();

        let mut all = Vec::new();
        for _ in 0..2 {
            all.extend(archiver.tick(&bases, &mut store).archived);
        }
        assert!(!all.contains(&2));
        assert!(all.contains(&1) && all.contains(&3));
        assert_eq!(store.len(), 2);
    }

    /// A `Never` policy entity is skipped; a `NextOnly` entity is archived once
    /// then never again.
    #[test]
    fn the_policy_controls_when_an_entity_is_archived() {
        let mut archiver = Archiver::new(1);
        archiver.set_auto_archive(1, AutoArchivePolicy::Never);
        archiver.set_auto_archive(2, AutoArchivePolicy::NextOnly);
        let bases = bases_of(&[persisted_base(1, 11), persisted_base(2, 22)]);
        let mut store = ArchiveStore::new();

        let first = archiver.tick(&bases, &mut store);
        assert!(!first.archived.contains(&1));
        assert!(first.archived.contains(&2));
        assert_eq!(archiver.auto_archive_policy(2), AutoArchivePolicy::Never);

        let second = archiver.tick(&bases, &mut store);
        assert!(!second.archived.contains(&1));
        assert!(!second.archived.contains(&2), "archived only once");
    }

    /// A BaseApp death is reported to BaseAppMgr when the cycle restarts, and
    /// is then cleared.
    #[test]
    fn a_death_is_reported_when_the_cycle_completes() {
        let mut archiver = Archiver::new(1);
        let bases = bases_of(&[persisted_base(1, 11)]);
        let mut store = ArchiveStore::new();
        assert!(!archiver.handle_base_app_death((9, 9), &bases));

        let outcome = archiver.tick(&bases, &mut store);
        assert_eq!(outcome.inform_archive_complete, Some((9, 9)));
        assert_eq!(
            archiver.tick(&bases, &mut store).inform_archive_complete,
            None,
            "the report is one-shot"
        );
    }

    /// With a secondary database a death restarts the cycle at once.
    #[test]
    fn a_death_restarts_the_cycle_with_a_secondary_db() {
        let mut archiver = Archiver::new(4).with_secondary_db(true);
        let bases = bases_of(&[persisted_base(1, 11)]);
        assert!(archiver.handle_base_app_death((9, 9), &bases));
    }

    /// The archived state survives a restart: a fresh archiver reads the store
    /// back and rebuilds the entity with the persisted properties.
    #[test]
    fn restart_recovers_the_persisted_state() {
        let mut base = persisted_base(42, 7);
        base.base_props
            .insert("health".to_string(), PropValue::Int(200));
        base.base_props
            .insert("name".to_string(), PropValue::Str("alice".to_string()));
        base.cell_props
            .insert("pos".to_string(), PropValue::Vec3([1.0, 2.0, 3.0]));

        let mut archiver = Archiver::new(1);
        let bases = bases_of(&[base.clone()]);
        let mut store = ArchiveStore::new();
        archiver.tick(&bases, &mut store);

        let ty = account_type();
        let restored = restore_archived(&store, 7, &lookup_of(ty))
            .expect("persisted")
            .expect("restored");
        assert_eq!(restored.id, 42);
        assert_eq!(restored.db_id, 7);
        assert_eq!(restored.base_props, base.base_props);
        assert_eq!(restored.cell_props, base.cell_props);
    }

    /// Nothing is returned for a database id that was never archived.
    #[test]
    fn an_unpersisted_id_has_nothing_to_restore() {
        let store = ArchiveStore::new();
        let ty = account_type();
        assert!(restore_archived(&store, 7, &lookup_of(ty)).is_none());
    }
}
