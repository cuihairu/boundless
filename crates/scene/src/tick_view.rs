//! The tick face of the scene views (`lib/scene/tick_scene_view.{hpp,cpp}`):
//! `ITickSceneProvider`, `ITickSceneListener`, `TickSceneView` and the
//! type-dispatched `TickOperation`.
//!
//! The reference stores raw provider/listener pointers; the port stores
//! `Rc` faces (`TickSceneView::providers_`/`listeners_`) and removes by
//! pointer identity (`Rc::ptr_eq` standing in for the erase-remove on the
//! pointer value). Provider mutation uses interior mutability on the
//! concrete type — the reference `tick` is a non-const member.

use std::rc::Rc;

use crate::scene_object::{SceneObject, SceneObjectOperation};
use crate::type_system::RuntimeTypeID;

/// `ITickSceneProvider` (`tick_scene_view.hpp:11-18`).
pub trait ITickSceneProvider {
    fn tick(&self, d_time: f32);
    fn update_animations(&self, d_time: f32);
}

/// `ITickSceneListener` (`tick_scene_view.hpp:20-25`).
pub trait ITickSceneListener {
    fn on_pre_tick(&mut self);
    fn on_post_tick(&mut self);
}

/// `TickSceneView` (`tick_scene_view.hpp:27-42`): listeners fire pre then
/// providers tick then listeners fire post (`tick_scene_view.cpp:8-27`);
/// `updateAnimations` reaches the providers only (cpp:29-38).
pub struct TickSceneView {
    /// `ISceneView::pOwner_` — the owning scene's identity token.
    p_owner: Option<crate::SceneToken>,
    providers: Vec<Rc<dyn ITickSceneProvider>>,
    listeners: Vec<Rc<std::cell::RefCell<dyn ITickSceneListener>>>,
}

impl TickSceneView {
    /// `ISceneView()` (`scene_provider.cpp:12-16`).
    pub fn new() -> Self {
        Self {
            p_owner: None,
            providers: Vec::new(),
            listeners: Vec::new(),
        }
    }

    /// `ISceneView::scene()` (`scene_provider.cpp:27-30`).
    pub fn scene(&self) -> Option<crate::SceneToken> {
        self.p_owner
    }

    /// `ISceneView::scene(pScene)` (`scene_provider.cpp:18-24`): remember
    /// the old owner, assign, then fire the hook.
    pub fn set_scene(&mut self, scene: Option<crate::SceneToken>) {
        let old_scene = self.p_owner;
        self.p_owner = scene;
        self.on_set_scene(old_scene, scene);
    }

    /// `ISceneView::onSetScene` (`scene_provider.cpp:33-36`): nothing by
    /// default.
    pub fn on_set_scene(
        &mut self,
        _old_scene: Option<crate::SceneToken>,
        _new_scene: Option<crate::SceneToken>,
    ) {
    }

    /// `SceneView<ITickSceneProvider>::addProvider(ProviderInterface*)`
    /// (`scene_provider.hpp:102-105`).
    pub fn add_provider(&mut self, provider: Rc<dyn ITickSceneProvider>) {
        self.providers.push(provider);
    }

    /// Collection size — the reference inspects `providers_` directly
    /// (port-added accessor for callers outside the crate-internal tests).
    pub fn providers_len(&self) -> usize {
        self.providers.len()
    }

    /// `removeProvider` erases by pointer identity
    /// (`scene_provider.hpp:107-115`).
    pub fn remove_provider(&mut self, provider: &Rc<dyn ITickSceneProvider>) {
        self.providers.retain(|p| !Rc::ptr_eq(p, provider));
    }

    /// `tick` (`tick_scene_view.cpp:8-27`).
    pub fn tick(&mut self, d_time: f32) {
        for listener in &self.listeners {
            listener.borrow_mut().on_pre_tick();
        }

        for provider in &self.providers {
            provider.tick(d_time);
        }

        for listener in &self.listeners {
            listener.borrow_mut().on_post_tick();
        }
    }

    /// `updateAnimations` (`tick_scene_view.cpp:29-38`).
    pub fn update_animations(&mut self, d_time: f32) {
        for provider in &self.providers {
            provider.update_animations(d_time);
        }
    }

    /// `addListener` (`tick_scene_view.cpp:40-43`).
    pub fn add_listener(&mut self, listener: Rc<std::cell::RefCell<dyn ITickSceneListener>>) {
        self.listeners.push(listener);
    }

    /// `removeListener` erases by pointer identity
    /// (`tick_scene_view.cpp:45-55`).
    pub fn remove_listener(&mut self, listener: &Rc<std::cell::RefCell<dyn ITickSceneListener>>) {
        self.listeners.retain(|l| !Rc::ptr_eq(l, listener));
    }
}

impl Default for TickSceneView {
    fn default() -> Self {
        Self::new()
    }
}

/// `ITickOperationTypeHandler` (`tick_scene_view.hpp:44-56`): the
/// `SceneObject* + count` pair arrives as a slice.
pub trait ITickOperationTypeHandler {
    fn do_tick(&mut self, objects: &mut [SceneObject], d_time: f32);
    fn do_update_animations(&mut self, objects: &mut [SceneObject], d_time: f32);
}

/// `TickOperation : SceneObjectOperation<ITickOperationTypeHandler>`
/// (`tick_scene_view.hpp:58-70`).
#[derive(Default)]
pub struct TickOperation {
    base: SceneObjectOperation<dyn ITickOperationTypeHandler>,
}

impl TickOperation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn base(&self) -> &SceneObjectOperation<dyn ITickOperationTypeHandler> {
        &self.base
    }

    pub fn base_mut(&mut self) -> &mut SceneObjectOperation<dyn ITickOperationTypeHandler> {
        &mut self.base
    }

    /// `tick(objects, count, dTime)` (`tick_scene_view.cpp:58-72`): each
    /// object is offered to its own type handler as a one-object slice.
    pub fn tick(&mut self, objects: &mut [SceneObject], d_time: f32) {
        for i in 0..objects.len() {
            let type_id = objects[i].type_id();
            if let Some(handler) = self.base.get_handler_mut_by_id(type_id) {
                handler.do_tick(&mut objects[i..i + 1], d_time);
            }
        }
    }

    /// `tick(typeID, objects, count, dTime)` (`tick_scene_view.cpp:74-85`):
    /// one handler for the whole slice.
    pub fn tick_typed(&mut self, type_id: RuntimeTypeID, objects: &mut [SceneObject], d_time: f32) {
        if let Some(handler) = self.base.get_handler_mut_by_id(type_id) {
            handler.do_tick(objects, d_time);
        }
    }

    /// `updateAnimations(objects, count, dTime)` (cpp:87-101).
    pub fn update_animations(&mut self, objects: &mut [SceneObject], d_time: f32) {
        for i in 0..objects.len() {
            let type_id = objects[i].type_id();
            if let Some(handler) = self.base.get_handler_mut_by_id(type_id) {
                handler.do_update_animations(&mut objects[i..i + 1], d_time);
            }
        }
    }

    /// `updateAnimations(typeID, objects, count, dTime)` (cpp:103-114).
    pub fn update_animations_typed(
        &mut self,
        type_id: RuntimeTypeID,
        objects: &mut [SceneObject],
        d_time: f32,
    ) {
        if let Some(handler) = self.base.get_handler_mut_by_id(type_id) {
            handler.do_update_animations(objects, d_time);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_object_flags::SceneObjectFlags;
    use std::cell::RefCell;

    struct Npc;
    struct Bullets;

    #[derive(Default)]
    struct RecordingProvider {
        ticks: RefCell<Vec<f32>>,
        animations: RefCell<Vec<f32>>,
    }

    impl ITickSceneProvider for RecordingProvider {
        fn tick(&self, d_time: f32) {
            self.ticks.borrow_mut().push(d_time);
        }
        fn update_animations(&self, d_time: f32) {
            self.animations.borrow_mut().push(d_time);
        }
    }

    #[derive(Default)]
    struct RecordingListener {
        events: RefCell<Vec<&'static str>>,
    }

    impl ITickSceneListener for RecordingListener {
        fn on_pre_tick(&mut self) {
            self.events.borrow_mut().push("pre");
        }
        fn on_post_tick(&mut self) {
            self.events.borrow_mut().push("post");
        }
    }

    #[derive(Default, Clone)]
    struct CountingHandler {
        ticked: Rc<RefCell<Vec<u64>>>,
        animated: Rc<RefCell<Vec<u64>>>,
    }

    impl CountingHandler {
        fn new() -> (Box<Self>, Self) {
            let handler = Self::default();
            (Box::new(handler.clone()), handler)
        }
    }

    impl ITickOperationTypeHandler for CountingHandler {
        fn do_tick(&mut self, objects: &mut [SceneObject], _d_time: f32) {
            for object in objects {
                self.ticked.borrow_mut().push(object.handle());
            }
        }
        fn do_update_animations(&mut self, objects: &mut [SceneObject], _d_time: f32) {
            for object in objects {
                self.animated.borrow_mut().push(object.handle());
            }
        }
    }

    fn object_of<T: ?Sized + 'static>(handle: u64) -> SceneObject {
        SceneObject::typed::<T>(handle, SceneObjectFlags::new())
    }

    #[test]
    fn tick_walks_pre_listeners_providers_then_post_listeners() {
        let mut view = TickSceneView::new();
        let provider = Rc::new(RecordingProvider::default());
        view.add_provider(provider.clone());

        let listener = Rc::new(RefCell::new(RecordingListener::default()));
        view.add_listener(listener.clone());

        view.tick(0.25);

        assert_eq!(
            listener.borrow().events.borrow().clone(),
            ["pre", "post"],
            "pre before post"
        );
        assert_eq!(*provider.ticks.borrow(), [0.25]);
        assert!(
            provider.animations.borrow().is_empty(),
            "tick must not animate"
        );

        view.update_animations(0.5);
        assert_eq!(*provider.animations.borrow(), [0.5]);
        assert_eq!(
            provider.ticks.borrow().len(),
            1,
            "animations leave ticks alone"
        );

        // Removing by identity drops exactly the matching face.
        let as_listener: Rc<RefCell<dyn ITickSceneListener>> = listener.clone();
        view.remove_listener(&as_listener);
        let as_provider: Rc<dyn ITickSceneProvider> = provider.clone();
        view.remove_provider(&as_provider);
        view.tick(1.0);
        assert_eq!(provider.ticks.borrow().len(), 1);
    }

    #[test]
    fn second_provider_joins_the_walk_in_collection_order() {
        let mut view = TickSceneView::new();
        let first = Rc::new(RecordingProvider::default());
        let second = Rc::new(RecordingProvider::default());
        view.add_provider(first.clone());
        view.add_provider(second.clone());
        view.tick(2.0);
        assert_eq!(*first.ticks.borrow(), [2.0]);
        assert_eq!(*second.ticks.borrow(), [2.0]);
    }

    #[test]
    fn tick_operation_dispatches_each_object_to_its_type_handler() {
        let mut operation = TickOperation::new();
        let (npc_box, npc) = CountingHandler::new();
        operation.base_mut().add_handler_for::<Npc>(npc_box);

        let mut objects = vec![
            object_of::<Npc>(1),
            object_of::<Bullets>(2),
            object_of::<Npc>(3),
        ];
        operation.tick(&mut objects, 0.1);

        assert_eq!(
            *npc.ticked.borrow(),
            [1, 3],
            "Bullets has no handler and is skipped"
        );

        // The typed face hands the whole slice to one handler.
        operation.tick_typed(SceneObject::type_of::<Npc>(), &mut objects, 0.2);
        assert_eq!(*npc.ticked.borrow(), [1, 3, 1, 2, 3]);

        // Unknown type ids are a no-op on both faces.
        operation.tick_typed(SceneObject::type_of::<Bullets>(), &mut objects, 0.3);
        assert_eq!(npc.ticked.borrow().len(), 5);
    }

    #[test]
    fn update_animations_follows_the_same_dispatch_shape() {
        let mut operation = TickOperation::new();
        let (npc_box, npc) = CountingHandler::new();
        operation.base_mut().add_handler_for::<Npc>(npc_box);

        let mut objects = vec![object_of::<Npc>(5), object_of::<Bullets>(6)];
        operation.update_animations(&mut objects, 0.4);
        assert_eq!(*npc.animated.borrow(), [5]);

        operation.update_animations_typed(SceneObject::type_of::<Npc>(), &mut objects, 0.5);
        assert_eq!(*npc.animated.borrow(), [5, 5, 6]);

        // Unknown type ids are a no-op on the typed animations face too.
        operation.update_animations_typed(SceneObject::type_of::<Bullets>(), &mut objects, 0.6);
        assert_eq!(npc.animated.borrow().len(), 3);
    }

    #[test]
    fn scene_reports_the_owner_token() {
        let mut view = TickSceneView::new();
        assert!(view.scene().is_none());
        view.set_scene(Some(9));
        assert_eq!(view.scene(), Some(9));
    }

    #[test]
    fn operation_base_face_answers_handler_presence() {
        let mut operation = TickOperation::new();
        assert!(
            !operation
                .base()
                .is_supported_type(SceneObject::type_of::<Npc>())
        );
        let (npc_box, _npc) = CountingHandler::new();
        operation.base_mut().add_handler_for::<Npc>(npc_box);
        assert!(
            operation
                .base()
                .is_supported_type(SceneObject::type_of::<Npc>())
        );
    }
}
