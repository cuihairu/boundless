//! BW-07-03-02: the BaseAppMgr's service-fragment map.
//!
//! A ServiceApp hosts *service entities*; each one registers its mailbox as a
//! *fragment* of a named service (`Base::registerServiceFragment`, `base.cpp:983`
//! → `BaseAppMgrGateway::registerServiceFragment`, `baseappmgr_gateway.cpp:126`,
//! which streams `type_description().name()` and `baseEntityMailBoxRef()` to
//! the BaseAppMgr). The BaseAppMgr keeps an authoritative
//! `ServicesMap` (`lib/server/services_map.hpp`): a `multimap<string,
//! EntityMailBoxRef>` of service name → fragments. `ServiceAppSubSet::
//! addServiceFragmentFromStream` (`baseappmgr.cpp:2755`) inserts the fragment,
//! broadcasts `BaseAppIntInterface::addServiceFragment` to every managed app
//! (the `ServiceAppSubSet` iterator yields service apps only, but `apps_` is
//! the whole managed set, so the message reaches every BaseApp and ServiceApp
//! including the sender), and forwards `CellAppMgrInterface::addServiceFragment`
//! to the CellAppMgr. `deregisterServiceFragment` (`baseappmgr.cpp:2812`)
//! removes the sender's fragment for the service, broadcasts
//! `delServiceFragment` and forwards it to the CellAppMgr; when a service loses
//! its last fragment and `Config::shutDownServerOnServiceDeath()` is set the
//! server shuts down (`didLoseService` and `shutDownStage_ == SHUTDOWN_NONE`).
//!
//! The same configuration produces the same available services: every app's
//! `ServicesMap` is filled from the same broadcasts, so `service_names()`,
//! `fragments_for_service()` and `choose_fragment()` agree. The reference picks
//! a fragment with `rand() % count` (`services_map.cpp`); this model takes the
//! random draw as an argument so the choice is deterministic and testable.

use crate::entity_creator::EntityMailBoxRef;

/// A fragment's owner address (a `Mercury::Address`-shaped `(ip, port)` pair).
pub type FragmentAddr = crate::baseappmgr::Addr;

/// The BaseAppMgr's service-fragment map: a service name to the fragments of
/// that service's entities, in insertion order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServicesMap {
    /// `fragments_`: service name → its fragments, in insertion order.
    fragments: std::collections::BTreeMap<String, Vec<EntityMailBoxRef>>,
}

impl ServicesMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// `ServicesMap::addFragment` (`services_map.cpp:44`).
    pub fn add_fragment(&mut self, service_name: &str, mailbox: EntityMailBoxRef) {
        self.fragments
            .entry(service_name.to_string())
            .or_default()
            .push(mailbox);
    }

    /// `ServicesMap::removeFragment` (`services_map.cpp:60`): remove the
    /// fragments of `service_name` whose address is `address`. Returns the
    /// number of fragments removed and whether that left the service with no
    /// fragments at all (`*pDidLoseService`).
    pub fn remove_fragment(&mut self, service_name: &str, address: FragmentAddr) -> (usize, bool) {
        let Some(fragments) = self.fragments.get_mut(service_name) else {
            return (0, false);
        };
        let before = fragments.len();
        fragments.retain(|mailbox| mailbox.addr != address);
        let removed = before - fragments.len();
        let did_lose = removed > 0 && fragments.is_empty();
        if fragments.is_empty() {
            self.fragments.remove(service_name);
        }
        (removed, did_lose)
    }

    /// `ServicesMap::removeFragmentsForAddress` (`services_map.cpp:96`):
    /// remove every fragment owned by `address`. Returns the number removed and
    /// the sorted names of the services that lost their last fragment
    /// (`*pLostServices`).
    pub fn remove_fragments_for_address(&mut self, address: FragmentAddr) -> (usize, Vec<String>) {
        let mut removed_total = 0;
        let mut lost = Vec::new();
        for key in self.fragments.keys().cloned().collect::<Vec<_>>() {
            if let Some(fragments) = self.fragments.get_mut(&key) {
                let before = fragments.len();
                fragments.retain(|mailbox| mailbox.addr != address);
                let removed = before - fragments.len();
                removed_total += removed;
                if removed > 0 && fragments.is_empty() {
                    lost.push(key.clone());
                    self.fragments.remove(&key);
                }
            }
        }
        (removed_total, lost)
    }

    /// `ServicesMap::getServiceNames` (`services_map.cpp:147`): the registered
    /// service names, sorted.
    pub fn service_names(&self) -> Vec<String> {
        self.fragments.keys().cloned().collect()
    }

    /// Whether `service_name` has any fragments.
    pub fn has_service(&self, service_name: &str) -> bool {
        self.fragments.contains_key(service_name)
    }

    /// `ServicesMap::getFragmentsForService` (`services_map.cpp:218`): the
    /// fragments of `service_name`, in insertion order.
    pub fn fragments_for_service(&self, service_name: &str) -> Vec<EntityMailBoxRef> {
        self.fragments
            .get(service_name)
            .cloned()
            .unwrap_or_default()
    }

    /// `ServicesMap::chooseFragment` (`services_map.cpp:186`): choose a
    /// fragment of `service_name` by `rand() % count`. `draw` is the value the
    /// RNG returned; the index is `draw % count`, reproducing the reference's
    /// uniform pick. Returns `None` when the service has no fragments.
    pub fn choose_fragment(&self, service_name: &str, draw: usize) -> Option<EntityMailBoxRef> {
        let fragments = self.fragments.get(service_name)?;
        if fragments.is_empty() {
            return None;
        }
        Some(fragments[draw % fragments.len()])
    }

    /// The total number of fragments across all services.
    pub fn len(&self) -> usize {
        self.fragments.values().map(Vec::len).sum()
    }

    /// Whether there are no fragments.
    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity_creator::{EntityMailBoxRef, MailBoxComponent};

    fn mailbox(id: u32, addr: FragmentAddr) -> EntityMailBoxRef {
        EntityMailBoxRef::init(id, addr, MailBoxComponent::Base, 1)
    }

    const SERVICE_B: u32 = 0x0100_0002;

    fn addr(i: u16) -> FragmentAddr {
        (1, 10_000 + i)
    }

    #[test]
    fn a_registered_fragment_is_an_available_service() {
        let mut map = ServicesMap::new();
        assert!(map.is_empty());

        map.add_fragment("DBAppMgr", mailbox(1, addr(1)));

        assert!(map.has_service("DBAppMgr"));
        assert_eq!(map.service_names(), vec!["DBAppMgr".to_string()]);
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.fragments_for_service("DBAppMgr"),
            vec![mailbox(1, addr(1))]
        );
    }

    #[test]
    fn the_same_configuration_produces_the_same_available_services() {
        // Run the identical registration sequence twice.
        fn run() -> ServicesMap {
            let mut map = ServicesMap::new();
            map.add_fragment("A", mailbox(1, addr(1)));
            map.add_fragment("B", mailbox(2, addr(2)));
            map.add_fragment("A", mailbox(3, addr(3)));
            map
        }
        let first = run();
        let second = run();

        // Same set of available services, same fragments per service.
        assert_eq!(first.service_names(), second.service_names());
        assert_eq!(
            first.service_names(),
            vec!["A".to_string(), "B".to_string()]
        );
        assert_eq!(
            first.fragments_for_service("A"),
            second.fragments_for_service("A")
        );
        assert_eq!(
            first.fragments_for_service("A"),
            vec![mailbox(1, addr(1)), mailbox(3, addr(3))]
        );
        assert_eq!(first.fragments_for_service("B"), vec![mailbox(2, addr(2))]);
        assert_eq!(first.len(), second.len());
    }

    #[test]
    fn a_service_may_have_several_fragments() {
        let mut map = ServicesMap::new();
        map.add_fragment("A", mailbox(1, addr(1)));
        map.add_fragment("A", mailbox(2, addr(2)));
        map.add_fragment("A", mailbox(3, addr(3)));
        assert_eq!(map.fragments_for_service("A").len(), 3);
        // Every draw lands on a real fragment.
        for draw in 0..6 {
            assert!(map.choose_fragment("A", draw).is_some());
        }
    }

    #[test]
    fn choose_fragment_indexes_by_draw_modulo_count() {
        let mut map = ServicesMap::new();
        map.add_fragment("A", mailbox(1, addr(1)));
        map.add_fragment("A", mailbox(2, addr(2)));
        map.add_fragment("A", mailbox(3, addr(3)));
        // `rand() % count`: draw 0,3 → index 0; 1,4 → 1; 2,5 → 2.
        assert_eq!(map.choose_fragment("A", 0), Some(mailbox(1, addr(1))));
        assert_eq!(map.choose_fragment("A", 3), Some(mailbox(1, addr(1))));
        assert_eq!(map.choose_fragment("A", 4), Some(mailbox(2, addr(2))));
        assert_eq!(map.choose_fragment("A", 5), Some(mailbox(3, addr(3))));
        // An unknown service has no fragment to choose.
        assert_eq!(map.choose_fragment("Missing", 0), None);
    }

    #[test]
    fn removing_a_service_address_drops_the_service() {
        let mut map = ServicesMap::new();
        map.add_fragment("A", mailbox(1, addr(1)));
        map.add_fragment("B", mailbox(2, addr(1)));
        // Removing addr(1) takes both services' only fragments.
        let (removed, lost) = map.remove_fragments_for_address(addr(1));
        assert_eq!(removed, 2);
        assert_eq!(lost, vec!["A".to_string(), "B".to_string()]);
        assert!(map.is_empty());
        assert_eq!(map.service_names(), Vec::<String>::new());
    }

    #[test]
    fn removing_one_fragment_of_a_service_keeps_it() {
        let mut map = ServicesMap::new();
        map.add_fragment("A", mailbox(1, addr(1)));
        map.add_fragment("A", mailbox(2, addr(2)));
        map.add_fragment("B", mailbox(3, addr(1)));
        // Remove addr(1): A keeps one fragment, B loses its only one.
        let (removed, lost) = map.remove_fragments_for_address(addr(1));
        assert_eq!(removed, 2);
        assert_eq!(lost, vec!["B".to_string()]);
        assert_eq!(map.fragments_for_service("A"), vec![mailbox(2, addr(2))]);
        assert!(!map.has_service("B"));
        assert_eq!(map.service_names(), vec!["A".to_string()]);
    }

    #[test]
    fn remove_fragment_reports_losing_the_last_fragment() {
        let mut map = ServicesMap::new();
        map.add_fragment("A", mailbox(1, addr(1)));
        map.add_fragment("A", mailbox(2, addr(2)));
        // Remove one of two: not lost.
        let (removed, did_lose) = map.remove_fragment("A", addr(1));
        assert_eq!(removed, 1);
        assert!(!did_lose);
        assert!(map.has_service("A"));
        // Remove the last one: lost.
        let (removed, did_lose) = map.remove_fragment("A", addr(2));
        assert_eq!(removed, 1);
        assert!(did_lose);
        assert!(!map.has_service("A"));
        // Removing an unknown service or an unknown address is a no-op.
        let (removed, did_lose) = map.remove_fragment("A", addr(2));
        assert_eq!((removed, did_lose), (0, false));
    }

    #[test]
    fn service_names_are_sorted() {
        let mut map = ServicesMap::new();
        map.add_fragment("Zulu", mailbox(1, addr(1)));
        map.add_fragment("Alpha", mailbox(2, addr(2)));
        map.add_fragment("Mike", mailbox(3, addr(3)));
        assert_eq!(
            map.service_names(),
            vec!["Alpha".to_string(), "Mike".to_string(), "Zulu".to_string()]
        );
    }

    #[test]
    fn fragments_for_an_unknown_service_is_empty() {
        let map = ServicesMap::new();
        assert!(map.fragments_for_service("Missing").is_empty());
        assert!(!map.has_service("Missing"));
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn a_service_map_can_be_built_from_broadcasts() {
        // Two managers fed the same broadcast sequence agree on the services.
        fn apply(map: &mut ServicesMap, name: &str, mb: EntityMailBoxRef) {
            map.add_fragment(name, mb);
        }
        let mut m1 = ServicesMap::new();
        let mut m2 = ServicesMap::new();
        let events = [
            ("A", mailbox(1, addr(1))),
            ("B", mailbox(2, addr(2))),
            ("A", mailbox(3, addr(1))),
        ];
        for (name, mb) in &events {
            apply(&mut m1, name, *mb);
            apply(&mut m2, name, *mb);
        }
        assert_eq!(m1.service_names(), m2.service_names());
        assert_eq!(m1.fragments_for_service("A"), m2.fragments_for_service("A"));
        // Both pick the same fragment for the same draw.
        assert_eq!(m1.choose_fragment("A", 0), m2.choose_fragment("A", 0));
    }

    #[test]
    fn the_map_holds_entity_mailbox_refs() {
        // The fragment carries the entity id, address, component and type id
        // of the registered service entity, exactly as streamed.
        let mut map = ServicesMap::new();
        let mb = EntityMailBoxRef::init(SERVICE_B, addr(1), MailBoxComponent::Base, 42);
        map.add_fragment("Service", mb);
        let got = map.fragments_for_service("Service")[0];
        assert_eq!(got.id, SERVICE_B);
        assert_eq!(got.component, MailBoxComponent::Base);
        assert_eq!(got.type_id, 42);
        // The fragment's address is the ServiceApp the entity runs on.
        assert_eq!(got.addr, addr(1));
    }
}
