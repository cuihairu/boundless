//! BW-07-03-01: global bases registration and lookup.
//!
//! A global base is a base reachable by a well-known key
//! (`BigWorld.globalBases[key]`). The BaseAppMgr holds the authoritative
//! map; each BaseApp keeps a local dictionary that answers reads.
//! `Base::registerGlobally` sends
//! `BaseAppMgrInterface::registerBaseGlobally(pickledKey, mailbox)` with a
//! reply handler (`GlobalBases::registerRequest`,
//! `global_bases.cpp:284`); the manager inserts the entry, broadcasts
//! `BaseAppIntInterface::addGlobalBase` to every *other* app
//! (`baseappmgr.cpp:2679-2689`) and answers the sender with an int8 success
//! code (1 inserted, 0 duplicate, `baseappmgr.cpp:2677-2701`). On a 1 the
//! `MakeGlobalHandler` adds the entry to the sender's own dictionary and
//! calls the script callback with `True` (`global_bases.cpp:223-250`); a 0
//! calls it with `False` and changes nothing. Reads therefore agree across
//! apps: the manager's map, the sender's dict and every other app's dict
//! hold the same mailbox.
//!
//! The reference pickles the key; this model uses the key string directly.
//! The restore-time re-registration path
//! (`GlobalBases::addLocalsToStream`/`recoverGlobalBasesFromStream`) and the
//! dead-app repair (`handleBaseAppDeath`/`resolveInvalidMailboxes`) are
//! tracked with the recovery lanes.

use crate::baseappmgr::GlobalBaseMailbox;
use crate::entity_creator::EntityId;
use std::collections::BTreeMap;

/// One BaseApp's `GlobalBases` dictionary: the local mirror of the
/// BaseAppMgr's authoritative map, filled by broadcasts and by the
/// registration acks for its own bases.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct GlobalBases {
    /// The dictionary: the (pickled) key -> the global base mailbox.
    map: BTreeMap<String, GlobalBaseMailbox>,
    /// `registeredBases_`: the bases registered by this app, id -> keys.
    registered: BTreeMap<EntityId, Vec<String>>,
}

/// The `Base::registerGlobally` request this app sends to the BaseAppMgr
/// (`GlobalBases::registerRequest`, `global_bases.cpp:284`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterRequest {
    /// The (pickled) key to register under.
    pub key: String,
    /// The base's `baseEntityMailBoxRef`.
    pub mailbox: GlobalBaseMailbox,
}

/// The `MakeGlobalHandler` outcome of the registration ack
/// (`global_bases.cpp:223-260`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterAck {
    /// Success code 1: the key was new; the entry is in the local
    /// dictionary and the callback got `True`.
    Registered,
    /// Success code 0: the key was taken; the callback got `False` and
    /// nothing changed.
    Duplicate,
    /// `handleException`: the reply channel failed; the callback got
    /// `False`.
    Exception,
}

/// The outcome of a `Base::deregisterGlobally`
/// (`GlobalBases::deregister`, `global_bases.cpp:336`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeregisterOutcome {
    /// This base owned the key: the entry dropped locally and the manager
    /// deregister request is issued.
    Deregistered,
    /// The key exists but no base registered it here.
    NotOwner,
    /// The key is not in the dictionary at all.
    Missing,
}

impl GlobalBases {
    /// Constructor.
    pub fn new() -> Self {
        Self::default()
    }

    /// `GlobalBases::add(Base*, pickledKey)` on a successful ack
    /// (`MakeGlobalHandler::handleMessage`): the app's own base joins the
    /// dictionary and is recorded for later deregistration.
    pub fn add_local(
        &mut self,
        base_id: EntityId,
        key: &str,
        mailbox: GlobalBaseMailbox,
    ) -> RegisterAck {
        self.map.insert(key.to_string(), mailbox);
        self.registered
            .entry(base_id)
            .or_default()
            .push(key.to_string());
        RegisterAck::Registered
    }

    /// `GlobalBases::add(BinaryIStream)` on an `addGlobalBase` broadcast
    /// (`global_bases.cpp:60`).
    pub fn add(&mut self, key: &str, mailbox: GlobalBaseMailbox) {
        self.map.insert(key.to_string(), mailbox);
    }

    /// `GlobalBases::remove` on a `delGlobalBase` broadcast
    /// (`global_bases.cpp:78`).
    pub fn remove(&mut self, key: &str) {
        self.map.remove(key);
    }

    /// The subscript / `get` / `has_key`: read the local dictionary.
    pub fn get(&self, key: &str) -> Option<&GlobalBaseMailbox> {
        self.map.get(key)
    }

    /// `len(globalBases)`.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether the dictionary is empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// `keys()` / `items()` keys.
    pub fn keys(&self) -> Vec<&str> {
        self.map.keys().map(String::as_str).collect()
    }

    /// The `Base::registerGlobally` request to send to the BaseAppMgr.
    pub fn register_request(key: &str, mailbox: GlobalBaseMailbox) -> RegisterRequest {
        RegisterRequest {
            key: key.to_string(),
            mailbox,
        }
    }

    /// The registration ack from the manager
    /// (`MakeGlobalHandler::handleMessage`, `global_bases.cpp:223`): on
    /// success the entry joins the dictionary and the base is recorded;
    /// on a duplicate or an exception the dictionary is untouched.
    pub fn on_register_ack(
        &mut self,
        base_id: EntityId,
        key: &str,
        mailbox: GlobalBaseMailbox,
        success: bool,
    ) -> RegisterAck {
        if success {
            self.add_local(base_id, key, mailbox)
        } else {
            RegisterAck::Duplicate
        }
    }

    /// The registration exception path (`handleException`): the callback
    /// got `False` and the dictionary is untouched.
    pub fn on_register_exception(&mut self) -> RegisterAck {
        RegisterAck::Exception
    }

    /// `GlobalBases::deregister(Base*, key)` (`global_bases.cpp:336`):
    /// only the base that registered a key may drop it. The entry is
    /// removed locally and the manager deregister request returned.
    pub fn deregister(&mut self, base_id: EntityId, key: &str) -> DeregisterOutcome {
        let Some(keys) = self.registered.get_mut(&base_id) else {
            return if self.map.contains_key(key) {
                DeregisterOutcome::NotOwner
            } else {
                DeregisterOutcome::Missing
            };
        };
        let Some(position) = keys.iter().position(|k| k == key) else {
            return if self.map.contains_key(key) {
                DeregisterOutcome::NotOwner
            } else {
                DeregisterOutcome::Missing
            };
        };
        keys.remove(position);
        if keys.is_empty() {
            self.registered.remove(&base_id);
        }
        self.map.remove(key);
        DeregisterOutcome::Deregistered
    }

    /// `GlobalBases::onBaseDestroyed` (`global_bases.cpp:398`): every key
    /// this base registered drops from the dictionary, and the keys to
    /// deregister at the manager are returned in registration order.
    pub fn on_base_destroyed(&mut self, base_id: EntityId) -> Vec<String> {
        let keys = self.registered.remove(&base_id).unwrap_or_default();
        for key in &keys {
            self.map.remove(key);
        }
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::baseappmgr::{BaseAppScheduler, RegisterOutcome};

    fn mailbox(entity: u32) -> GlobalBaseMailbox {
        GlobalBaseMailbox {
            entity,
            addr: (1, 10001),
        }
    }

    /// A scheduler with two ready BaseApps (the sender and a peer).
    fn scheduler_with_two_apps() -> (BaseAppScheduler, Addr, Addr) {
        let mut scheduler = BaseAppScheduler::new();
        scheduler.set_cell_app_mgr_established(true);
        scheduler.set_has_init_data(true);
        scheduler.set_db_apps_empty(false);
        for i in [1u16, 2] {
            let addr = cell_addr(i);
            assert!(matches!(
                scheduler.register(addr, client_addr(i), false, 10),
                RegisterOutcome::Registered(_)
            ));
            scheduler.finished_init(addr);
        }
        (scheduler, cell_addr(1), cell_addr(2))
    }

    #[test]
    fn a_registered_global_base_reads_the_same_on_every_base_app() {
        let (mut scheduler, sender, peer) = scheduler_with_two_apps();

        // The sender's base asks the manager to register it.
        let request = GlobalBases::register_request("MainMission", mailbox(42));
        assert_eq!(request.key, "MainMission");

        let mut sender_bases = GlobalBases::new();
        let mut peer_bases = GlobalBases::new();

        let (success, broadcasts) =
            scheduler.register_base_globally(sender, &request.key, request.mailbox.clone());
        // The key was new: success 1, and every app except the sender is
        // told to add it.
        assert_eq!(success, 1);
        assert_eq!(
            broadcasts,
            vec![(
                peer,
                ToBaseApp::AddGlobalBase {
                    name: "MainMission".to_string(),
                    mailbox: mailbox(42),
                }
            )]
        );

        // The sender learns from the ack; the peer from the broadcast.
        assert_eq!(
            sender_bases.on_register_ack(42, &request.key, mailbox(42), success > 0),
            RegisterAck::Registered
        );
        for (_, message) in broadcasts {
            match message {
                ToBaseApp::AddGlobalBase { name, mailbox } => {
                    peer_bases.add(&name, mailbox);
                }
                _ => panic!("expected an add broadcast"),
            }
        }

        // Cross-BaseApp reads agree.
        assert_eq!(sender_bases.get("MainMission"), Some(&mailbox(42)));
        assert_eq!(peer_bases.get("MainMission"), Some(&mailbox(42)));
    }

    #[test]
    fn a_duplicate_registration_changes_nothing() {
        let (mut scheduler, sender, _peer) = scheduler_with_two_apps();
        let (success, broadcasts) =
            scheduler.register_base_globally(sender, "MainMission", mailbox(42));
        assert_eq!(success, 1);
        assert_eq!(broadcasts.len(), 1);

        // A second app tries the same key: success 0, no broadcast.
        let (success, broadcasts) =
            scheduler.register_base_globally(cell_addr(2), "MainMission", mailbox(77));
        assert_eq!(success, 0);
        assert!(broadcasts.is_empty());

        let mut bases = GlobalBases::new();
        bases.add("MainMission", mailbox(42));
        // The duplicate's callback got False and the entry is untouched.
        assert_eq!(
            bases.on_register_ack(77, "MainMission", mailbox(77), false),
            RegisterAck::Duplicate
        );
        assert_eq!(bases.get("MainMission"), Some(&mailbox(42)));
    }

    #[test]
    fn an_exception_answer_leaves_the_dictionary_untouched() {
        let mut bases = GlobalBases::new();
        assert_eq!(bases.on_register_exception(), RegisterAck::Exception);
        assert!(bases.get("MainMission").is_none());
    }

    #[test]
    fn a_deregistered_key_is_gone_on_every_base_app() {
        let (mut scheduler, sender, peer) = scheduler_with_two_apps();
        let (success, _) = scheduler.register_base_globally(sender, "MainMission", mailbox(42));
        assert_eq!(success, 1);

        let mut sender_bases = GlobalBases::new();
        let mut peer_bases = GlobalBases::new();
        sender_bases.add_local(42, "MainMission", mailbox(42));
        peer_bases.add("MainMission", mailbox(42));

        // The owner deregisters: the manager drops it and tells the peers.
        let broadcasts = scheduler.deregister_base_globally(sender, "MainMission");
        assert_eq!(
            broadcasts,
            vec![(
                peer,
                ToBaseApp::DelGlobalBase {
                    name: "MainMission".to_string(),
                }
            )]
        );
        assert_eq!(
            sender_bases.deregister(42, "MainMission"),
            DeregisterOutcome::Deregistered
        );
        for (_, message) in broadcasts {
            match message {
                ToBaseApp::DelGlobalBase { name } => peer_bases.remove(&name),
                _ => panic!("expected a delete broadcast"),
            }
        }
        assert_eq!(sender_bases.get("MainMission"), None);
        assert_eq!(peer_bases.get("MainMission"), None);
        assert!(sender_bases.is_empty());
    }

    #[test]
    fn only_the_registering_base_may_deregister_a_key() {
        let mut bases = GlobalBases::new();
        bases.add_local(42, "MainMission", mailbox(42));
        // A different base id owns nothing.
        assert_eq!(
            bases.deregister(77, "MainMission"),
            DeregisterOutcome::NotOwner
        );
        // The key is still readable.
        assert!(bases.get("MainMission").is_some());
        // An unknown key is missing.
        assert_eq!(
            bases.deregister(77, "OtherMission"),
            DeregisterOutcome::Missing
        );
    }

    #[test]
    fn a_destroyed_base_drops_its_registered_keys() {
        let mut bases = GlobalBases::new();
        bases.add_local(42, "MainMission", mailbox(42));
        bases.add_local(42, "OtherMission", mailbox(43));
        bases.add_local(77, "PeerMission", mailbox(44));
        assert_eq!(bases.len(), 3);

        let dropped = bases.on_base_destroyed(42);
        // The keys drop in registration order.
        assert_eq!(
            dropped,
            vec!["MainMission".to_string(), "OtherMission".to_string()]
        );
        assert!(bases.get("MainMission").is_none());
        assert!(bases.get("OtherMission").is_none());
        assert!(bases.get("PeerMission").is_some());
        assert_eq!(bases.len(), 1);
        // A second notification is empty.
        assert!(bases.on_base_destroyed(42).is_empty());
    }

    #[test]
    fn keys_and_len_track_the_dictionary() {
        let mut bases = GlobalBases::new();
        assert!(bases.is_empty());
        bases.add("b", mailbox(2));
        bases.add("a", mailbox(1));
        assert_eq!(bases.len(), 2);
        assert_eq!(bases.keys(), vec!["a", "b"]);
        bases.remove("a");
        assert_eq!(bases.keys(), vec!["b"]);
    }

    fn cell_addr(i: u16) -> Addr {
        (1, 1000 + i)
    }

    fn client_addr(i: u16) -> Addr {
        (1, 2000 + i)
    }

    use crate::baseappmgr::Addr;
    use crate::baseappmgr::ToBaseApp;
}
