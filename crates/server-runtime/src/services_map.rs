//! BW-07-03-02 / BW-07-03-03: the BaseAppMgr's service-fragment map and the
//! dispatch of a service call.
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
//!
//! A service call is `BigWorld.services[serviceName].method(args)`
//! (`PyServicesMap`, `lib/server/py_services_map.hpp`; each BaseApp and
//! CellApp keeps its own map filled from the broadcasts, `baseapp.cpp:1342`
//! puts it on the `BigWorld` module). The subscript chooses a fragment and
//! hands back its mailbox; calling a method on that mailbox is an ordinary
//! `MailboxMethodCall` to the fragment's entity
//! (`EntityCreator::deliver_mailbox_call`). Every instance of a service runs
//! the same type description, so the same call behaves the same whichever
//! fragment is chosen — only the target entity differs.

use crate::entity_creator::{EntityMailBoxRef, PropValue};
use crate::mailbox_call::MailboxMethodCall;

/// A fragment's owner address (a `Mercury::Address`-shaped `(ip, port)` pair).
pub type FragmentAddr = crate::baseappmgr::Addr;

/// A dispatched service call (`BigWorld.services[name].method(...)`): the
/// chosen fragment and the mailbox method call to deliver to it.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceDispatch {
    /// The fragment the subscript picked (`chooseFragment`).
    pub target: EntityMailBoxRef,
    /// The method call to deliver to `target`.
    pub call: MailboxMethodCall,
}

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

    /// Dispatch a service call (`BigWorld.services[serviceName]` subscript,
    /// `py_services_map.cpp:31`, then `PyEntityMailBox::callMethod`,
    /// `mailbox_base.cpp:149`): choose a fragment by `rand() % count` and
    /// build the mailbox method call to deliver to it. The call carries the
    /// fragment's id as the target and the method's arguments; services are
    /// not exposed cell methods, so no source entity id is written. `draw` is
    /// the RNG value. Returns `None` when the service has no fragments (a
    /// `KeyError` on the subscript).
    pub fn dispatch_service_call(
        &self,
        service_name: &str,
        draw: usize,
        method_index: u32,
        args: Vec<PropValue>,
    ) -> Option<ServiceDispatch> {
        let target = self.choose_fragment(service_name, draw)?;
        Some(ServiceDispatch {
            target,
            call: MailboxMethodCall {
                target_id: target.id,
                method_index,
                source_entity_id: None,
                args,
            },
        })
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

    // ------------------------------------------------------------------
    // BW-07-03-03 分发服务调用
    // ------------------------------------------------------------------

    use crate::entity_creator::{
        BaseCallback, EntityCreator, EntityType, IdPool, MethodComponent, MethodDesc,
    };

    /// A service entity type with one base method `process(int, string)`.
    fn service_type() -> EntityType {
        EntityType {
            id: 1,
            name: "Service".to_string(),
            can_be_on_base: true,
            is_service: true,
            is_proxy: false,
            can_be_on_cell: false,
            properties: vec![],
            methods: vec![MethodDesc::new(
                "process",
                3,
                MethodComponent::Base,
                false,
                vec![PropValue::Int(0), PropValue::Str(String::new())],
                vec![],
            )],
        }
    }

    /// A creator holding two instances of the service entity, started the way
    /// a ServiceApp starts them (`ServiceStarter::run`).
    fn creator_with_two_services() -> EntityCreator {
        let mut pool = IdPool::new(5, 10, 50, 100);
        pool.refill(&[1, 2]);
        let mut creator = EntityCreator::new(pool);
        creator.add_type(service_type());
        for _ in 0..2 {
            creator.create_service("Service").expect("create");
        }
        creator
    }

    /// The service fragment map of the two instances, each on its own
    /// ServiceApp address.
    fn service_map(creator: &EntityCreator) -> ServicesMap {
        let mut ids: Vec<u32> = creator.bases().keys().copied().collect();
        ids.sort();
        let mut map = ServicesMap::new();
        for (index, id) in ids.iter().enumerate() {
            map.add_fragment(
                "A",
                EntityMailBoxRef::init(*id, addr(index as u16 + 1), MailBoxComponent::Service, 1),
            );
        }
        map
    }

    /// A service call goes to the instance the draw selects, carrying the
    /// fragment's id and the method's arguments.
    #[test]
    fn a_service_call_targets_the_chosen_fragment() {
        let creator = &creator_with_two_services();
        let map = service_map(creator);
        let args = vec![PropValue::Int(1)];

        // `rand() % 2`: draw 0 the first fragment, draw 1 the second.
        let first = map
            .dispatch_service_call("A", 0, 3, args.clone())
            .expect("dispatch");
        let second = map
            .dispatch_service_call("A", 1, 3, args.clone())
            .expect("dispatch");

        let fragments = map.fragments_for_service("A");
        assert_eq!(first.target, fragments[0]);
        assert_eq!(second.target, fragments[1]);
        assert_ne!(first.target.id, second.target.id);
        // The call targets the fragment and carries the arguments.
        assert_eq!(first.call.target_id, first.target.id);
        assert_eq!(first.call.source_entity_id, None); // not a cell method
        assert_eq!(first.call.args, args);
        assert_eq!(first.call.method_index, 3);
    }

    /// Every instance of the service runs the same type description, so the
    /// same call behaves the same whichever fragment is chosen: the delivered
    /// method and arguments match, only the target entity differs.
    #[test]
    fn a_service_call_behaves_the_same_on_every_instance() {
        let mut creator = creator_with_two_services();
        let map = service_map(&creator);
        let args = vec![PropValue::Int(7), PropValue::Str("ping".to_string())];

        // The service has two instances (fragments) to compare.
        let fragments = map.fragments_for_service("A");
        assert_eq!(fragments.len(), 2);
        let ids: Vec<_> = fragments.iter().map(|f| f.id).collect();

        // The same call dispatched to each instance the draws select.
        for draw in 0..2 {
            let dispatch = map
                .dispatch_service_call("A", draw, 3, args.clone())
                .expect("dispatch");
            assert_eq!(
                dispatch.target,
                fragments[draw % fragments.len()],
                "draw {draw} lands on a real instance"
            );
            creator
                .deliver_mailbox_call(&dispatch.target, dispatch.call)
                .expect("deliver");
        }

        // Each instance received the same method with the same arguments (the
        // creation callbacks are filtered out).
        let callbacks: Vec<_> = creator
            .bases()
            .values()
            .flat_map(|base| base.callbacks.iter())
            .filter(|callback| matches!(callback, BaseCallback::MethodCalled { .. }))
            .collect();
        assert_eq!(callbacks.len(), 2, "both instances got one call");
        for callback in &callbacks {
            let BaseCallback::MethodCalled {
                entity_id,
                method_name,
                method_index,
                source_entity_id,
                args: got,
            } = callback
            else {
                panic!("unexpected callback: {callback:?}");
            };
            assert!(
                ids.contains(entity_id),
                "the call landed on a service instance"
            );
            assert_eq!(method_name, "process");
            assert_eq!(*method_index, 3);
            assert_eq!(*source_entity_id, None);
            assert_eq!(got, &args, "the instances behave the same");
        }
        // Each draw landed on its own instance.
        let mut got_ids: Vec<_> = creator
            .bases()
            .values()
            .flat_map(|base| {
                base.callbacks.iter().filter_map(|cb| match cb {
                    BaseCallback::MethodCalled { entity_id, .. } => Some(*entity_id),
                    _ => None,
                })
            })
            .collect();
        got_ids.sort();
        let mut want_ids = ids.clone();
        want_ids.sort();
        assert_eq!(got_ids, want_ids, "one call per instance");
    }

    /// A service call on a missing service is a `KeyError`: no fragment to
    /// dispatch to.
    #[test]
    fn a_service_call_on_a_missing_service_is_a_key_error() {
        let map = ServicesMap::new();
        assert!(
            map.dispatch_service_call("Missing", 0, 3, vec![PropValue::Int(1)])
                .is_none()
        );
        // An empty service counts as missing too.
        assert!(map.dispatch_service_call("A", 0, 3, vec![]).is_none());
    }

    /// The same draw on the same map always dispatches to the same instance.
    #[test]
    fn the_same_draw_dispatches_to_the_same_instance() {
        let creator = &creator_with_two_services();
        let map = service_map(creator);
        let args = vec![PropValue::Int(3)];
        for draw in 0..4 {
            let one = map
                .dispatch_service_call("A", draw, 3, args.clone())
                .expect("dispatch");
            let two = map
                .dispatch_service_call("A", draw, 3, args.clone())
                .expect("dispatch");
            assert_eq!(one.target, two.target);
            assert_eq!(one.call, two.call);
        }
    }
}
