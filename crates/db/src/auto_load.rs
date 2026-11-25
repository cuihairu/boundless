//! The auto-load restore face: which entities the database hands back to
//! a starting DBApp, and how a put changes that set.
//!
//! Three pieces, straight off the reference:
//!
//! 1. [`UpdateAutoLoad`] — the put-time tri-state
//!    (`common.hpp:95-103`): a successful `putEntity` may set the row's
//!    auto-load flag, clear it, or leave it alone (`xml_database.cpp:700-711`).
//!    A delete always clears it (`xml_database.cpp:837`).
//! 2. [`EntityStorage::auto_load_entities`] — `IDatabase::autoLoadEntities`
//!    (`idatabase.hpp:357`): harvest the flagged ids, feed them to a
//!    loader, then start it. The XML backend walks its
//!    `_BigWorldInfo/AutoLoadedEntities` ids and prunes the ones whose
//!    rows are gone (`xml_database.cpp:1059-1095`); the MySQL backend
//!    joins `bigworldLogOns` on `shouldAutoLoad` and, when there are
//!    results, blanks every stored base address (`mysql_database.cpp:667-706`,
//!    `UPDATE bigworldLogOns SET ip = 0, port = 0`) so a restored entity
//!    never inherits a dead socket.
//! 3. [`EntityAutoLoader`] / [`PacedEntityAutoLoader`] —
//!    `IEntityAutoLoader` (`entity_auto_loader_interface.hpp`) and the
//!    pacing core of `EntityAutoLoader` (`entity_auto_loader.cpp:199-275`):
//!    at most `maxOutstanding` entities load concurrently, one failure
//!    stops further sends, and an empty set reports "nothing auto
//!    loaded" rather than a completed load.
//!
//! The per-entity work itself — the reference's `AutoLoadingEntityHandler`
//! get → set-base-to-LOGGING_ON → createBase → set-base-to-final chain
//! (`entity_auto_loader.cpp:84-93`) — rides on the Mercury and BaseAppMgr
//! faces and stays outside this crate: the caller's load closure plays
//! the handler, the crate supplies the id stream, the pacing and the
//! error accounting. The acceptance for BW-09-03-04 is the restored set:
//! after a restart the loader is handed exactly the entities that were
//! flagged when the database was written.

use foundation::ids::{DatabaseId, EntityTypeId};

/// The put-time auto-load tri-state (`common.hpp:95-103`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpdateAutoLoad {
    /// Don't modify the auto load state.
    #[default]
    Retain,
    /// Set the auto-load state to true.
    True,
    /// Set the auto-load state to false.
    False,
}

/// `IEntityAutoLoader` (`entity_auto_loader_interface.hpp`): the
/// database's side of the contract. `auto_load_entities` feeds entities
/// in and then starts (or aborts) the load; the implementation decides
/// what "loading" means.
pub trait EntityAutoLoader {
    /// `IEntityAutoLoader::reserve`: a capacity hint for `num_entities`
    /// incoming entities. Only the MySQL backend bothers
    /// (`mysql_database.cpp:696`); a no-op default is faithful.
    fn reserve(&mut self, _num_entities: usize) {}

    /// `IEntityAutoLoader::start`: load everything handed over so far.
    fn start(&mut self);

    /// `IEntityAutoLoader::abort`: the harvest failed — nothing loads.
    fn abort(&mut self);

    /// `IEntityAutoLoader::addEntity`: queue `(typeID, dbID)` for the
    /// load (`entity_auto_loader.cpp:232-235`).
    fn add_entity(&mut self, type_id: EntityTypeId, db_id: DatabaseId);
}

/// `DBApp::Config::maxConcurrentEntityLoaders`' default
/// (`dbapp_config.cpp:22`).
pub const DEFAULT_MAX_OUTSTANDING: usize = 5;

/// The pacing core of the server's `EntityAutoLoader`
/// (`entity_auto_loader.cpp:170-275`): it queues entities, keeps at most
/// `max_outstanding` in flight, stops sending the moment one load fails
/// (`if (!hasErrors_) sendNext()`), and reports whether a load actually
/// started. The per-entity `load` closure plays
/// `AutoLoadingEntityHandler` — in the reference it reports back
/// asynchronously through `onAutoLoadEntityComplete`; the synchronous
/// port calls it the moment `load` returns, which collapses the
/// handler→manager callback chain into `send_next` itself.
pub struct PacedEntityAutoLoader<F>
where
    F: FnMut(EntityTypeId, DatabaseId) -> bool,
{
    load: F,
    /// `maxOutstanding_`.
    max_outstanding: usize,
    /// `entities_`.
    entities: Vec<(EntityTypeId, DatabaseId)>,
    /// `numOutstanding_`.
    num_outstanding: usize,
    /// `numSent_`.
    num_sent: usize,
    /// `hasErrors_`.
    has_errors: bool,
    /// The `start()` outcome: `None` until started or aborted, then
    /// whether any load was begun — `onEntitiesAutoLoadCompleted(false)`
    /// for the empty set (`entity_auto_loader.cpp:200-208`).
    did_auto_load: Option<bool>,
}

impl<F> PacedEntityAutoLoader<F>
where
    F: FnMut(EntityTypeId, DatabaseId) -> bool,
{
    /// A loader with `DBApp::Config::maxConcurrentEntityLoaders`' default
    /// concurrency (`entity_auto_loader.cpp:166-168`).
    pub fn new(load: F) -> Self {
        Self::with_max_outstanding(load, DEFAULT_MAX_OUTSTANDING)
    }

    /// A loader with an explicit concurrency, the way the server option
    /// overrides the default.
    pub fn with_max_outstanding(load: F, max_outstanding: usize) -> Self {
        Self {
            load,
            max_outstanding,
            entities: Vec::new(),
            num_outstanding: 0,
            num_sent: 0,
            has_errors: false,
            did_auto_load: None,
        }
    }

    /// Whether `start` found any entities to load — the
    /// `onEntitiesAutoLoadCompleted(didAutoLoad)` payload.
    pub fn did_auto_load(&self) -> bool {
        self.did_auto_load.unwrap_or(false)
    }

    /// Whether any load failed. One failure stops the pump
    /// (`entity_auto_loader.cpp:276-285`), the way
    /// `onEntitiesAutoLoadError` shuts the DBApp down.
    pub fn has_errors(&self) -> bool {
        self.has_errors
    }

    /// How many entities were handed to `load`.
    pub fn num_sent(&self) -> usize {
        self.num_sent
    }

    /// `EntityAutoLoader::sendNext` (`entity_auto_loader.cpp:237-253`):
    /// gate on the queue and the concurrency window, send one, and
    /// account its completion. The gate also carries `hasErrors_`: in
    /// the reference a failure only stops the completion-driven pump
    /// (`if (!hasErrors_) sendNext()`), and the start loop's remaining
    /// iterations run before any completion arrives — so no send ever
    /// happens after the first failure. The synchronous collapse makes
    /// that explicit.
    fn send_next(&mut self) {
        let all_sent = self.num_sent >= self.entities.len();
        if !self.has_errors && !all_sent && self.num_outstanding < self.max_outstanding {
            let (type_id, db_id) = self.entities[self.num_sent];
            self.num_sent += 1;
            self.num_outstanding += 1;
            let is_ok = (self.load)(type_id, db_id);
            // AutoLoadingEntityHandler's destructor calls
            // onAutoLoadEntityComplete (`entity_auto_loader.cpp:41-44`).
            self.on_auto_load_entity_complete(is_ok);
        }
    }

    /// `EntityAutoLoader::onAutoLoadEntityComplete`
    /// (`entity_auto_loader.cpp:276-285`): one failure stops further
    /// sends; successes keep the pump going.
    fn on_auto_load_entity_complete(&mut self, is_ok: bool) {
        self.num_outstanding -= 1;
        self.has_errors = self.has_errors || !is_ok;

        if !self.has_errors {
            self.send_next();
        }
    }
}

impl<F> EntityAutoLoader for PacedEntityAutoLoader<F>
where
    F: FnMut(EntityTypeId, DatabaseId) -> bool,
{
    fn reserve(&mut self, num_entities: usize) {
        self.entities.reserve(num_entities);
    }

    fn start(&mut self) {
        if self.entities.is_empty() {
            // "If no entities need to be loaded, just start the database
            // server" (`entity_auto_loader.cpp:200-208`).
            self.did_auto_load = Some(false);
            return;
        }
        self.did_auto_load = Some(true);
        for _ in 0..self.max_outstanding {
            self.send_next();
        }
    }

    fn abort(&mut self) {
        self.entities.clear();
        self.has_errors = true;
        self.did_auto_load = Some(false);
    }

    fn add_entity(&mut self, type_id: EntityTypeId, db_id: DatabaseId) {
        self.entities.push((type_id, db_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// The acceptance for BW-09-03-04, loader side: the harvest feeds
    /// every flagged id in, the paced load delivers exactly that set,
    /// and an empty flag set reports "nothing auto loaded".
    #[test]
    fn the_loader_delivers_every_flagged_entity_in_order() {
        let loaded = Rc::new(RefCell::new(Vec::new()));
        let sink = loaded.clone();
        let mut loader = PacedEntityAutoLoader::new(move |type_id, db_id| {
            sink.borrow_mut().push((type_id, db_id));
            true
        });
        loader.reserve(3);
        loader.add_entity(1, 10);
        loader.add_entity(1, 11);
        loader.add_entity(2, 12);
        loader.start();

        assert!(!loader.has_errors());
        assert!(loader.did_auto_load());
        assert_eq!(loader.num_sent(), 3);
        assert_eq!(*loaded.borrow(), vec![(1, 10), (1, 11), (2, 12)]);
    }

    #[test]
    fn an_empty_set_reports_no_auto_load() {
        let mut loader =
            PacedEntityAutoLoader::new(|_: EntityTypeId, _: DatabaseId| panic!("nothing loads"));
        loader.start();

        assert!(!loader.did_auto_load());
        assert!(!loader.has_errors());
        assert_eq!(loader.num_sent(), 0);
    }

    #[test]
    fn one_failure_stops_the_pump() {
        // The second entity fails: the third is never handed over, the
        // way hasErrors_ keeps sendNext from firing
        // (`entity_auto_loader.cpp:279-282`).
        let loaded = Rc::new(RefCell::new(Vec::new()));
        let sink = loaded.clone();
        let mut loader = PacedEntityAutoLoader::new(move |type_id, db_id| {
            sink.borrow_mut().push((type_id, db_id));
            db_id != 11
        });
        loader.add_entity(1, 10);
        loader.add_entity(1, 11);
        loader.add_entity(1, 12);
        loader.start();

        assert!(loader.has_errors());
        assert_eq!(*loaded.borrow(), vec![(1, 10), (1, 11)]);
        assert_eq!(loader.num_sent(), 2);
    }

    #[test]
    fn the_concurrency_window_paces_the_sends() {
        // maxOutstanding_ = 2 bounds the pump. The synchronous collapse
        // runs the window back to back, so what this pins is the visit
        // order (queue order, `entities_[numSent_]`) and that an explicit
        // window still drains every entity.
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        let mut loader = PacedEntityAutoLoader::with_max_outstanding(
            move |type_id, db_id| {
                sink.borrow_mut().push(format!("load {type_id}:{db_id}"));
                true
            },
            2,
        );
        loader.add_entity(1, 10);
        loader.add_entity(1, 11);
        loader.add_entity(1, 12);
        loader.start();

        assert_eq!(
            *events.borrow(),
            vec!["load 1:10", "load 1:11", "load 1:12"]
        );
        assert_eq!(loader.num_sent(), 3);
        assert!(!loader.has_errors());
    }

    /// The BW-09-03-04 acceptance: after a restart the loader is handed
    /// exactly the entities that were flagged when the database was
    /// written — a True flag restores, a False flag does not, Retain
    /// inherits whatever was there.
    #[test]
    fn a_restart_restores_exactly_the_flagged_set() {
        use crate::storage::{EntityKey, EntityStorage, MemoryStorage};
        use entitydef::value::PropertyValue;
        use foundation::ids::PENDING_DATABASE_ID;

        let mut store = MemoryStorage::new();
        let avatar = |db_id| EntityKey { type_id: 1, db_id };
        // Two rows ride in through the create leg: Avatar 1 flagged,
        // Avatar 2 unflagged.
        let one = store
            .put_entity(
                &avatar(PENDING_DATABASE_ID),
                &[PropertyValue::String("One".into())],
                false,
                UpdateAutoLoad::True,
            )
            .expect("the first create puts");
        let _two = store
            .put_entity(
                &avatar(PENDING_DATABASE_ID),
                &[PropertyValue::String("Two".into())],
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the second create puts");
        // An update leg flips a third row on and then off again.
        let three = store
            .put_entity(
                &avatar(PENDING_DATABASE_ID),
                &[PropertyValue::String("Three".into())],
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the third create puts");
        store
            .put_entity(
                &avatar(three),
                &[PropertyValue::String("Three".into())],
                false,
                UpdateAutoLoad::True,
            )
            .expect("the flag on");
        store
            .put_entity(
                &avatar(three),
                &[PropertyValue::String("Three".into())],
                false,
                UpdateAutoLoad::False,
            )
            .expect("the flag off");

        let loaded = Rc::new(RefCell::new(Vec::new()));
        let sink = loaded.clone();
        let mut loader = PacedEntityAutoLoader::new(move |type_id, db_id| {
            sink.borrow_mut().push((type_id, db_id));
            true
        });
        store.auto_load_entities(&mut loader).expect("the harvest");

        assert_eq!(*loaded.borrow(), vec![(1, one)]);
        assert!(loader.did_auto_load());
        assert!(!loader.has_errors());
    }

    /// A flagged row deleted before the restart is not restored: the
    /// delete clears the flag with the row (`xml_database.cpp:837`).
    #[test]
    fn a_delete_takes_the_flag_with_the_row() {
        use crate::storage::{EntityKey, EntityStorage, MemoryStorage};
        use entitydef::value::PropertyValue;
        use foundation::ids::PENDING_DATABASE_ID;

        let mut store = MemoryStorage::new();
        let avatar = |db_id| EntityKey { type_id: 1, db_id };
        let doomed = store
            .put_entity(
                &avatar(PENDING_DATABASE_ID),
                &[PropertyValue::String("Doomed".into())],
                false,
                UpdateAutoLoad::True,
            )
            .expect("the create puts");
        store.del_entity(&avatar(doomed)).expect("the delete");

        let mut loader =
            PacedEntityAutoLoader::new(|_: EntityTypeId, _: DatabaseId| panic!("nothing loads"));
        store.auto_load_entities(&mut loader).expect("the harvest");
        assert!(!loader.did_auto_load(), "the flagged set is empty");
    }

    /// The XML restart cycle: a flagged entity written out and parsed
    /// back lands in the restored set; the `_BigWorldInfo` ids carry it.
    #[test]
    fn an_xml_restart_hands_back_the_flagged_entities() {
        use crate::storage::{EntityKey, EntityStorage as _};
        use crate::xml::{XmlDatabase, XmlKind, XmlPropDef, XmlTypeDef};
        use entitydef::value::PropertyValue;
        use foundation::ids::PENDING_DATABASE_ID;

        let def = |name: &'static str, type_id| XmlTypeDef {
            name: name.to_owned(),
            type_id,
            properties: vec![XmlPropDef {
                name: "playerName".to_owned(),
                kind: XmlKind::String,
            }],
            identifier: Some(0),
        };
        let defs = || vec![def("Avatar", 1), def("Goblin", 2)];

        let mut database = XmlDatabase::create(defs());
        let avatar = database
            .put_entity(
                &EntityKey {
                    type_id: 1,
                    db_id: PENDING_DATABASE_ID,
                },
                &[PropertyValue::String("Kept".into())],
                false,
                UpdateAutoLoad::True,
            )
            .expect("the avatar puts");
        let _goblin = database
            .put_entity(
                &EntityKey {
                    type_id: 2,
                    db_id: PENDING_DATABASE_ID,
                },
                &[PropertyValue::String("Gone".into())],
                false,
                UpdateAutoLoad::Retain,
            )
            .expect("the goblin puts");

        // The restart: the document is the database's whole state.
        let mut restarted = XmlDatabase::parse(&database.to_xml(), defs()).expect("it parses");
        assert_eq!(restarted.auto_load_ids(), vec![avatar]);

        let loaded = Rc::new(RefCell::new(Vec::new()));
        let sink = loaded.clone();
        let mut loader = PacedEntityAutoLoader::new(move |type_id, db_id| {
            sink.borrow_mut().push((type_id, db_id));
            true
        });
        restarted
            .auto_load_entities(&mut loader)
            .expect("the harvest");
        assert_eq!(*loaded.borrow(), vec![(1, avatar)]);
        assert_eq!(restarted.auto_load_ids(), vec![avatar]);
    }

    /// `XMLDatabase::autoLoadEntities` prunes a flag whose row is gone
    /// (`xml_database.cpp:1088-1095`) instead of delivering it.
    #[test]
    fn a_flag_without_a_row_is_pruned_not_delivered() {
        use crate::storage::EntityStorage as _;
        use crate::xml::{XmlDatabase, XmlKind, XmlPropDef, XmlTypeDef};

        let doc = concat!(
            "<db.xml>\n",
            "\t<_BigWorldInfo>\n",
            "\t\t<AutoLoadedEntities>\n",
            "\t\t\t<databaseID>\t99\t</databaseID>\n",
            "\t\t\t<databaseID>\t3\t</databaseID>\n",
            "\t\t</AutoLoadedEntities>\n",
            "\t</_BigWorldInfo>\n",
            "\t<Avatar>\n",
            "\t\t<databaseID>\t3\t</databaseID>\n",
            "\t\t<playerName>\tKept\t</playerName>\n",
            "\t</Avatar>\n",
            "</db.xml>\n"
        );
        let defs = || {
            vec![XmlTypeDef {
                name: "Avatar".to_owned(),
                type_id: 1,
                properties: vec![XmlPropDef {
                    name: "playerName".to_owned(),
                    kind: XmlKind::String,
                }],
                identifier: Some(0),
            }]
        };
        let mut database = XmlDatabase::parse(doc, defs()).expect("it parses");
        assert_eq!(database.auto_load_ids(), vec![3, 99]);

        let loaded = Rc::new(RefCell::new(Vec::new()));
        let sink = loaded.clone();
        let mut loader = PacedEntityAutoLoader::new(move |type_id, db_id| {
            sink.borrow_mut().push((type_id, db_id));
            true
        });
        database
            .auto_load_entities(&mut loader)
            .expect("the harvest");
        assert_eq!(*loaded.borrow(), vec![(1, 3)]);
        assert_eq!(database.auto_load_ids(), vec![3], "the dead flag is pruned");
    }

    /// The MySQL restore blanks every stored base address when the
    /// flag set is non-empty (`mysql_database.cpp:703`), so a restored
    /// entity never inherits a dead socket.
    #[test]
    fn restoring_blanks_the_stored_base_addresses() {
        use crate::storage::{BaseMailbox, CheckOut, EntityKey, EntityStorage, MemoryStorage};
        use entitydef::value::PropertyValue;
        use foundation::ids::PENDING_DATABASE_ID;
        use mercury::endpoint::Address;

        let mut store = MemoryStorage::new();
        let avatar = |db_id| EntityKey { type_id: 1, db_id };
        let flagged = store
            .put_entity(
                &avatar(PENDING_DATABASE_ID),
                &[PropertyValue::String("Kept".into())],
                false,
                UpdateAutoLoad::True,
            )
            .expect("the create puts");
        store
            .check_out(
                &avatar(flagged),
                BaseMailbox {
                    addr: Address {
                        ip: 0x0102_0304,
                        port: 20933,
                        salt: 7,
                    },
                    entity_id: 42,
                },
            )
            .expect("the checkout grants");

        let mut loader = PacedEntityAutoLoader::new(|_: EntityTypeId, _: DatabaseId| true);
        store.auto_load_entities(&mut loader).expect("the harvest");

        let holder = store.logon_of(&avatar(flagged)).expect("the logon reads");
        let holder = holder.expect("still checked out");
        assert_eq!(holder.addr.ip, 0);
        assert_eq!(holder.addr.port, 0);
        assert!(matches!(
            store.check_out(&avatar(flagged), holder),
            Ok(CheckOut::Elsewhere(_))
        ));
    }

    #[test]
    fn an_abort_clears_the_queue_and_reports_the_error() {
        let mut loader =
            PacedEntityAutoLoader::new(|_: EntityTypeId, _: DatabaseId| panic!("nothing loads"));
        loader.add_entity(1, 10);
        loader.abort();

        assert!(!loader.did_auto_load());
        assert!(loader.has_errors());
        loader.start();
        assert_eq!(loader.num_sent(), 0, "the cleared queue sends nothing");
    }
}
