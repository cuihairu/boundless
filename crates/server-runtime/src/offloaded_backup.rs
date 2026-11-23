//! BW-07-04-03: handle offloaded backups.
//!
//! During retirement a BaseApp becomes the backup for the entities it offloads
//! to other BaseApps instead of sending them a backup copy
//! (`BaseApp::backupBaseEntity`, `baseapp.cpp:2656`): the bytes are kept here
//! keyed by the app the entity was offloaded to. When that app dies the
//! entities are re-offloaded to their backup BaseApp per the backup hash chain
//! (`BackedUpEntities::restoreRemotely`, `backed_up_base_app.cpp:203`), and
//! then the whole container is dropped — once the cluster has recovered from
//! one death it cannot recover from another, so everything is cleared to tie
//! up loose ends regarding offload confirmation.
//!
//! The cleanup of the old backup happens through the ack path
//! (`offloaded_backups.cpp:76`): the receiving BaseApp re-backs up the
//! offloaded entity (`backupBaseNow`) and on success sends
//! `ackOffloadBackup` back to the original source (`sendAckOffloadBackup`,
//! `baseapp.cpp:2451`), whose `ackOffloadBackup` handler calls
//! `stopBackingUpEntity` (`baseapp.cpp:2464`) — the source ceases to back up
//! the entity itself.
//!
//! This module models the container and both death and ack paths. The stored
//! bytes are the `writeBackupData` payload carried by [`BackupMessage`]; the
//! source app and the entity id are the container keys, matching the reference
//! `apps_` map of address to `BackedUpEntities`.

use crate::backup_hash::BackupHashChain;
use crate::base_backup::BackupMessage;
use crate::baseappmgr::Addr;
use crate::entity_creator::EntityId;

/// The offloaded backups held by a retiring BaseApp (`OffloadedBackups`,
/// `offloaded_backups.cpp`): the raw backup bytes per destination app, then
/// per entity id.
#[derive(Debug, Clone, Default)]
pub struct OffloadedBackups {
    /// The backups by the app the entity was offloaded to, then by entity id.
    apps: std::collections::BTreeMap<Addr, std::collections::BTreeMap<EntityId, Vec<u8>>>,
}

impl OffloadedBackups {
    /// An empty container.
    pub fn new() -> Self {
        Self::default()
    }

    /// `OffloadedBackups::backUpEntity` (`offloaded_backups.cpp:25`): store the
    /// backup bytes for an entity that was offloaded to `src_addr`. A newer
    /// backup of the same entity replaces the older one.
    pub fn back_up_entity(&mut self, src_addr: Addr, message: &BackupMessage) -> EntityId {
        self.apps
            .entry(src_addr)
            .or_default()
            .insert(message.entity_id, message.data.clone());
        message.entity_id
    }

    /// `OffloadedBackups::wasOffloaded` (`offloaded_backups.cpp:58`): whether
    /// any destination app holds an offloaded backup of the entity.
    pub fn was_offloaded(&self, entity_id: EntityId) -> bool {
        self.apps.values().any(|app| app.contains_key(&entity_id))
    }

    /// `OffloadedBackups::handleBaseAppDeath` (`offloaded_backups.cpp:42`):
    /// re-offload every entity backed up under the dead app to its backup
    /// BaseApp according to the hash chain (`restoreRemotely`,
    /// `backed_up_base_app.cpp:203`), then drop every backup. The returned
    /// messages are the offloads to send, each flagged `is_offload`.
    pub fn handle_base_app_death(
        &mut self,
        dead_addr: Addr,
        chain: &BackupHashChain,
    ) -> Vec<BackupMessage> {
        let offloads = match self.apps.get(&dead_addr) {
            Some(stored) => stored
                .iter()
                .map(|(id, data)| BackupMessage {
                    dest: chain.address_for(dead_addr, *id),
                    is_offload: true,
                    entity_id: *id,
                    data: data.clone(),
                })
                .collect(),
            None => Vec::new(),
        };
        // "Once the cluster has recovered from one death, it cannot recover
        // from another. Thus, clear the backups completely to tie up any loose
        // ends regarding offload confirmation."
        self.apps.clear();
        offloads
    }

    /// `OffloadedBackups::stopBackingUpEntity` (`offloaded_backups.cpp:76`):
    /// another BaseApp has taken responsibility for backing the entity up, so
    /// cease to back it up ourselves; an app whose entries are exhausted is
    /// dropped.
    pub fn stop_backing_up_entity(&mut self, entity_id: EntityId) {
        self.apps.retain(|_, app| {
            app.remove(&entity_id);
            !app.is_empty()
        });
    }

    /// Whether any offloaded backup is held.
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    /// The held backups, by destination app then entity id.
    pub fn apps(
        &self,
    ) -> &std::collections::BTreeMap<Addr, std::collections::BTreeMap<EntityId, Vec<u8>>> {
        &self.apps
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup_hash::BackupHash;

    fn offload_message(entity_id: EntityId, dest: Addr) -> BackupMessage {
        BackupMessage {
            dest,
            is_offload: true,
            entity_id,
            data: vec![entity_id as u8; 4],
        }
    }

    /// A chain that sends the entity's backup for a dead app to the given live
    /// app, mirroring the reference test setup.
    fn chain_redirecting(dead: Addr, live: Addr) -> BackupHashChain {
        let mut hash = BackupHash::with_prime(0x9e377e55);
        hash.push_back(live);
        let mut chain = BackupHashChain::new();
        chain.adjust_for_dead_base_app(dead, hash);
        chain
    }

    /// `backUpEntity` stores the bytes keyed by the app the entity was offloaded
    /// to, so `wasOffloaded` can find it in any destination's entry.
    #[test]
    fn a_backup_is_stored_under_the_destination_app() {
        let mut offloaded = OffloadedBackups::new();
        offloaded.back_up_entity((2, 2000), &offload_message(42, (2, 2000)));

        assert!(offloaded.was_offloaded(42));
        assert!(!offloaded.was_offloaded(7));
        assert!(!offloaded.is_empty());
        assert_eq!(
            offloaded.apps().get(&(2, 2000)).map(|app| app.len()),
            Some(1)
        );
    }

    /// `wasOffloaded` scans every destination app, not just the first.
    #[test]
    fn was_offloaded_scans_every_destination() {
        let mut offloaded = OffloadedBackups::new();
        offloaded.back_up_entity((2, 2000), &offload_message(7, (2, 2000)));
        offloaded.back_up_entity((3, 3000), &offload_message(42, (3, 3000)));

        assert!(offloaded.was_offloaded(42));
        assert!(offloaded.was_offloaded(7));
        assert!(!offloaded.was_offloaded(99));
    }

    /// A death re-offloads the dead app's entities to their backup BaseApp per
    /// the hash chain, each flagged as an offload.
    #[test]
    fn a_dead_apps_entities_are_reoffloaded_through_the_chain() {
        let dead = (2, 2000);
        let mut offloaded = OffloadedBackups::new();
        offloaded.back_up_entity(dead, &offload_message(42, dead));

        let chain = chain_redirecting(dead, (3, 3000));
        let offloads = offloaded.handle_base_app_death(dead, &chain);

        assert_eq!(offloads.len(), 1);
        assert_eq!(offloads[0].dest, (3, 3000));
        assert!(offloads[0].is_offload);
        assert_eq!(offloads[0].entity_id, 42);
    }

    /// After the death every backup is dropped, even ones held under other
    /// destinations — the cluster cannot recover from a second death.
    #[test]
    fn a_death_clears_every_backup() {
        let mut offloaded = OffloadedBackups::new();
        offloaded.back_up_entity((2, 2000), &offload_message(42, (2, 2000)));
        offloaded.back_up_entity((3, 3000), &offload_message(7, (3, 3000)));

        let chain = chain_redirecting((2, 2000), (3, 3000));
        let _ = offloaded.handle_base_app_death((2, 2000), &chain);

        assert!(offloaded.is_empty());
        assert!(offloaded.apps().is_empty());
    }

    /// `stopBackingUpEntity` removes the entity from every destination and
    /// drops a destination whose entries are exhausted, leaving others intact.
    #[test]
    fn stopping_backup_cleans_the_old_backup() {
        let mut offloaded = OffloadedBackups::new();
        offloaded.back_up_entity((2, 2000), &offload_message(42, (2, 2000)));
        offloaded.back_up_entity((3, 3000), &offload_message(42, (3, 3000)));
        offloaded.back_up_entity((3, 3000), &offload_message(7, (3, 3000)));

        offloaded.stop_backing_up_entity(42);

        assert!(!offloaded.was_offloaded(42));
        assert!(offloaded.was_offloaded(7));
        // The first destination is now empty and is dropped; the second keeps
        // its remaining entity.
        assert_eq!(offloaded.apps().len(), 1);
        assert_eq!(offloaded.apps().get(&(2, 2000)).map(|app| app.len()), None);
        assert_eq!(
            offloaded.apps().get(&(3, 3000)).map(|app| app.len()),
            Some(1)
        );
    }

    /// A death of an app we never offloaded to still clears every backup.
    #[test]
    fn a_death_of_an_unknown_app_still_clears() {
        let mut offloaded = OffloadedBackups::new();
        offloaded.back_up_entity((2, 2000), &offload_message(42, (2, 2000)));

        let chain = chain_redirecting((9, 9), (3, 3000));
        let offloads = offloaded.handle_base_app_death((9, 9), &chain);

        assert!(offloads.is_empty());
        assert!(offloaded.is_empty());
    }
}
