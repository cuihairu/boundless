//! Auto-load entity recovery after a restart.
//!
//! On startup the DBApp resolves the `AutoLoadedEntities` section of the
//! database and replays each listed entity back into the world
//! (`server/dbapp/entity_auto_loader.{hpp,cpp}`). This module is that
//! driver, decoupled from the network: the per-entity load step is a
//! caller-supplied sink standing in for the getEntity/createBase chain,
//! while the loader keeps the reference's bounded-concurrency queue and
//! completion accounting.
//!
//! Layering: domain library (layer 2) on top of [`crate::xml_database`]'s
//! auto-load resolution.

use foundation::ids::{DatabaseId, EntityTypeId};

/// The reference's loader bound
/// (`DBApp::Config::maxConcurrentEntityLoaders`,
/// `dbapp_config.cpp:22`).
pub const MAX_CONCURRENT_ENTITY_LOADERS: usize = 5;

/// How the recovery run ended (`EntityAutoLoader::checkFinished` /
/// `EntityAutoLoader::start` / `abort`,
/// `entity_auto_loader.cpp:232-269`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// No entities were listed: the caller just starts the database
    /// server, `did_auto_load` false.
    NothingToLoad,
    /// Every listed entity was sent and the last load reported success.
    Completed {
        /// `numSent_ > 0`.
        did_auto_load: bool,
    },
    /// A load failed: no further entities are sent and the caller is
    /// told the auto-load errored.
    Errored,
}

/// The restart auto-load driver (`EntityAutoLoader`,
/// `entity_auto_loader.cpp:171-269`).
pub struct EntityAutoLoader {
    entities: Vec<(EntityTypeId, DatabaseId)>,
    num_outstanding: usize,
    max_outstanding: usize,
    num_sent: usize,
    has_errors: bool,
    outcome: Option<RecoveryOutcome>,
}

impl EntityAutoLoader {
    /// `max_outstanding` is the loader bound
    /// (`DBApp::Config::maxConcurrentEntityLoaders`).
    pub fn new(max_outstanding: usize) -> Self {
        Self {
            entities: Vec::new(),
            num_outstanding: 0,
            max_outstanding,
            num_sent: 0,
            has_errors: false,
            outcome: None,
        }
    }

    /// Add a database entry to load later
    /// (`EntityAutoLoader::addEntity`,
    /// `entity_auto_loader.cpp:206-209`).
    pub fn add_entity(&mut self, type_id: EntityTypeId, db_id: DatabaseId) {
        self.entities.push((type_id, db_id));
    }

    /// How many entities are queued.
    pub fn pending(&self) -> usize {
        self.entities.len()
    }

    /// `true` when every queued entity has been handed to the sink
    /// (`EntityAutoLoader::allSent`,
    /// `entity_auto_loader.hpp`).
    pub fn all_sent(&self) -> bool {
        self.num_sent == self.entities.len()
    }

    /// How many entities were handed to the load sink (`numSent_`).
    pub fn num_sent(&self) -> usize {
        self.num_sent
    }

    /// Start loading: the first `max_outstanding` entities go out
    /// immediately (`EntityAutoLoader::start`,
    /// `entity_auto_loader.cpp:186-204`). `load` stands in for the
    /// getEntity/createBase chain and returns whether the entity loaded.
    pub fn start(&mut self, load: &mut dyn FnMut(EntityTypeId, DatabaseId) -> bool) {
        if self.entities.is_empty() {
            // If no entities need to be loaded, just start the database
            // server (`entity_auto_loader.cpp:188-192`).
            self.outcome = Some(RecoveryOutcome::NothingToLoad);
            return;
        }

        for _ in 0..self.max_outstanding {
            self.send_next(load);
        }
    }

    /// Load the next pending entity while under the bound. A loader with
    /// errors is finished — the reference deletes it in `checkFinished`,
    /// so no further entity is ever sent
    /// (`entity_auto_loader.cpp:211-229` and `:249-268`).
    fn send_next(&mut self, load: &mut dyn FnMut(EntityTypeId, DatabaseId) -> bool) {
        if !self.has_errors && !self.all_sent() && self.num_outstanding < self.max_outstanding {
            let (type_id, db_id) = self.entities[self.num_sent];
            let is_ok = load(type_id, db_id);
            self.num_sent += 1;
            self.num_outstanding += 1;
            self.on_auto_load_entity_complete(is_ok, load);
        }
    }

    /// The load step reported completion — success or failure
    /// (`EntityAutoLoader::onAutoLoadEntityComplete`,
    /// `entity_auto_loader.cpp:234-245`).
    pub fn on_auto_load_entity_complete(
        &mut self,
        is_ok: bool,
        load: &mut dyn FnMut(EntityTypeId, DatabaseId) -> bool,
    ) {
        self.num_outstanding -= 1;
        self.has_errors = self.has_errors || !is_ok;

        if !self.has_errors {
            self.send_next(load);
        }

        self.check_finished(load);
    }

    /// When nothing is outstanding the run is over
    /// (`EntityAutoLoader::checkFinished`,
    /// `entity_auto_loader.cpp:249-268`).
    fn check_finished(&mut self, _load: &mut dyn FnMut(EntityTypeId, DatabaseId) -> bool) {
        if self.num_outstanding == 0 && self.outcome.is_none() {
            if self.has_errors {
                self.outcome = Some(RecoveryOutcome::Errored);
            } else if self.all_sent() {
                self.outcome = Some(RecoveryOutcome::Completed {
                    did_auto_load: self.num_sent > 0,
                });
            }
        }
    }

    /// The run's outcome once it has finished.
    pub fn outcome(&self) -> Option<&RecoveryOutcome> {
        self.outcome.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml_database::tests::{avatar_stream, defs};
    use crate::xml_database::{AutoLoadUpdate, XmlDatabase};
    use foundation::stream::BinaryStream;
    use std::path::PathBuf;

    /// A database with two auto-load Avatars saved to disk — the state a
    /// crashed process leaves behind.
    fn seeded_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "boundless-autoload-{}-{name}.xml",
            std::process::id()
        ));
        let mut database = XmlDatabase::new(defs());
        let avatar = defs().name_to_index("Avatar").unwrap();
        for (db_id, hp) in [(1, 10), (2, 20)] {
            let mut stream = avatar_stream(hp, 0);
            database
                .put_entity_stream(avatar, db_id, &mut stream, AutoLoadUpdate::True, true)
                .expect("put");
        }
        std::fs::write(&path, database.to_xml()).expect("write");
        path
    }

    /// Reopen the saved file and resolve the auto-load list — the restart
    /// itself (`XMLDatabase::autoLoadEntities`).
    fn restart(path: &PathBuf) -> (XmlDatabase, Vec<(EntityTypeId, DatabaseId)>) {
        let xml = std::fs::read_to_string(path).expect("read");
        let _ = std::fs::remove_file(path);
        let mut database = XmlDatabase::from_xml(&xml, defs()).expect("reopen");
        let recovered = database.auto_load_entities();
        (database, recovered)
    }

    fn loader_for(recovered: &[(EntityTypeId, DatabaseId)]) -> EntityAutoLoader {
        let mut loader = EntityAutoLoader::new(MAX_CONCURRENT_ENTITY_LOADERS);
        for &(type_id, db_id) in recovered {
            loader.add_entity(type_id, db_id);
        }
        loader
    }

    #[test]
    fn a_restart_recovers_the_same_entity_set() {
        let (mut database, recovered) = restart(&seeded_path("restart-set"));

        assert_eq!(
            recovered,
            vec![(0, 1), (0, 2)],
            "the restarted set must match the pre-restart baseline"
        );

        // The loader replays exactly that set, in order.
        let mut loaded = Vec::new();
        let mut loader = loader_for(&recovered);
        loader.start(&mut |type_id, db_id| {
            let outcome =
                database.get_entity_stream(type_id, db_id, None, &mut BinaryStream::new());
            if outcome.found {
                loaded.push((type_id, db_id));
            }
            outcome.found
        });

        assert_eq!(
            loader.outcome(),
            Some(&RecoveryOutcome::Completed {
                did_auto_load: true
            })
        );
        assert_eq!(loaded, vec![(0, 1), (0, 2)]);
    }

    #[test]
    fn a_failed_load_stops_the_queue_and_reports_an_error() {
        let (_database, recovered) = restart(&seeded_path("restart-error"));

        // The first load fails: no further entity is sent
        // (`entity_auto_loader.cpp:239-241`).
        let mut loader = loader_for(&recovered);
        loader.start(&mut |_type_id, _db_id| false);

        assert_eq!(loader.outcome(), Some(&RecoveryOutcome::Errored));
        assert_eq!(loader.num_sent(), 1);
    }

    #[test]
    fn an_empty_database_starts_without_autoloading() {
        let mut loader = EntityAutoLoader::new(MAX_CONCURRENT_ENTITY_LOADERS);
        loader.start(&mut |_, _| panic!("nothing to load"));

        assert_eq!(
            loader.outcome(),
            Some(&RecoveryOutcome::NothingToLoad),
            "just start the database server"
        );
    }

    #[test]
    fn the_queue_refills_under_the_concurrency_bound() {
        // Six entities with a bound of two: each completion pulls the
        // next one in, so all six land despite the small window
        // (`entity_auto_loader.cpp:211-229`).
        let mut loader = EntityAutoLoader::new(2);
        for db_id in 1..=6 {
            loader.add_entity(0, db_id);
        }
        let mut loaded = Vec::new();
        loader.start(&mut |type_id, db_id| {
            loaded.push((type_id, db_id));
            true
        });

        assert_eq!(
            loader.outcome(),
            Some(&RecoveryOutcome::Completed {
                did_auto_load: true
            })
        );
        assert_eq!(loaded, (1..=6).map(|db_id| (0, db_id)).collect::<Vec<_>>());
    }

    #[test]
    fn the_recovered_database_answers_lookups_by_name() {
        // Post-restart world: the entity is back under its identifier.
        let (mut database, recovered) = restart(&seeded_path("restart-name"));

        let avatar = defs().name_to_index("Avatar").unwrap();
        assert_eq!(recovered, vec![(avatar, 1), (avatar, 2)]);
        let outcome = database.get_entity_stream(avatar, 1, None, &mut BinaryStream::new());
        assert!(outcome.found);
        assert_eq!(outcome.name.as_deref(), Some("avatar-10"));
    }
}
