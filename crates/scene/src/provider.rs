//! The provider/view delegation (`lib/scene/scene_provider.{hpp,cpp}` and
//! `lib/scene/scene.{hpp,cpp}`).
//!
//! Ownership adaptations (registered): the reference passes raw `Scene*`
//! and `ProviderInterface*` pointers through a shared graph; the port
//! hands a scene identity token to providers and `Rc` faces to views, and
//! keeps the reference's asserts (add-once, scene must be null on add,
//! add/remove order) and dispatch order verbatim. The reference's
//! `getView` returns an erased `void*`; the port's `SceneViewRef` carries
//! the typed faces (tick, collision, intersect). Views are stored twice —
//! once for the iteration face (`ISceneView`), once for the typed fetch
//! face — because `Rc<RefCell<dyn ISceneView>>` cannot downcast to a
//! concrete view.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::collision_operation::CollisionOperation;
use crate::collision_scene_view::{CollisionSceneView, ICollisionSceneViewProvider};
use crate::intersect_scene_view::{IIntersectSceneViewProvider, IntersectSceneView};
use crate::object_change_scene_view::ObjectChangeSceneView;
use crate::tick_view::{ITickSceneListener, ITickSceneProvider, TickOperation, TickSceneView};
use crate::type_system::{RuntimeTypeID, object_operation_runtime_id, scene_view_runtime_id};

/// The `Scene*` stand-in: a process-unique non-zero token (0 is reserved
/// for NULL, matching the reference's null checks).
pub type SceneToken = u64;

static NEXT_SCENE_TOKEN: AtomicU64 = AtomicU64::new(1);

fn take_scene_token() -> SceneToken {
    NEXT_SCENE_TOKEN.fetch_add(1, Ordering::Relaxed)
}

/// The id a tick view asks for and a provider answers under: the
/// interface key (`SceneTypeSystem::getSceneViewRuntimeID<ITickSceneProvider>()`
/// in the reference).
pub fn tick_provider_view_id() -> RuntimeTypeID {
    scene_view_runtime_id::<dyn ITickSceneProvider>()
}

/// The collision view's interface key
/// (`getSceneViewRuntimeID<ICollisionSceneViewProvider>()`).
pub fn collision_provider_view_id() -> RuntimeTypeID {
    scene_view_runtime_id::<dyn ICollisionSceneViewProvider>()
}

/// The intersect view's interface key
/// (`getSceneViewRuntimeID<IIntersectSceneViewProvider>()`).
pub fn intersect_provider_view_id() -> RuntimeTypeID {
    scene_view_runtime_id::<dyn IIntersectSceneViewProvider>()
}

/// `SceneProvider::getView`'s erased `void*` result, carrying the typed
/// faces a view may collect.
pub enum SceneViewRef {
    Tick(Rc<dyn ITickSceneProvider>),
    Collision(Rc<dyn ICollisionSceneViewProvider>),
    Intersect(Rc<dyn IIntersectSceneViewProvider>),
}

/// `SceneProvider::exposeView<Target>(pHost, typeID, outInterface)`
/// (`scene_provider.hpp:37-47`): fill `out` only when it is still empty
/// and the asked id matches.
pub fn expose_tick_view(
    host: Rc<dyn ITickSceneProvider>,
    type_id: RuntimeTypeID,
    out: &mut Option<SceneViewRef>,
) {
    if out.is_none() && type_id == tick_provider_view_id() {
        *out = Some(SceneViewRef::Tick(host));
    }
}

/// The collision view's expose face (same contract).
pub fn expose_collision_view(
    host: Rc<dyn ICollisionSceneViewProvider>,
    type_id: RuntimeTypeID,
    out: &mut Option<SceneViewRef>,
) {
    if out.is_none() && type_id == collision_provider_view_id() {
        *out = Some(SceneViewRef::Collision(host));
    }
}

/// The intersect view's expose face (same contract).
pub fn expose_intersect_view(
    host: Rc<dyn IIntersectSceneViewProvider>,
    type_id: RuntimeTypeID,
    out: &mut Option<SceneViewRef>,
) {
    if out.is_none() && type_id == intersect_provider_view_id() {
        *out = Some(SceneViewRef::Intersect(host));
    }
}

/// The faces a provider needs from its scene, materialized by
/// [`Scene::add_provider`] and handed to the provider through
/// [`SceneProvider::on_scene_bound`].
///
/// The reference's `DynamicSceneProvider::onSetScene` pulls these from the
/// scene through the `Scene*` it receives (`pNewScene->getView<...>()`,
/// `getObjectOperation<...>()`); the port cannot re-enter the scene while
/// the provider's `RefCell` is mutably borrowed, so the scene pushes them
/// instead. Materialization happens *before* the provider joins the scene
/// collection, which reproduces the reference's effective order — the
/// pull-time view walk misses the new provider because `addProvider`
/// appends it only after `onSetScene` returns.
pub struct SceneBoundFaces {
    pub tick_view: Rc<RefCell<TickSceneView>>,
    pub tick_operation: Rc<RefCell<TickOperation>>,
    pub collision_operation: Rc<RefCell<CollisionOperation>>,
    pub object_change_view: Rc<RefCell<ObjectChangeSceneView>>,
}

/// `SceneProvider` (`scene_provider.hpp:13-51`). The reference stores
/// `pScene_` in the base class and fires `onSetScene` from
/// `scene(pScene)`; concrete port providers embed [`ProviderSceneRef`]
/// and forward, keeping the hook contract identical.
pub trait SceneProvider {
    /// `scene()` (`scene_provider.cpp:67-76`).
    fn scene(&self) -> Option<SceneToken>;

    /// `scene(pScene)` (`scene_provider.cpp:52-58`): remember the old
    /// scene, assign, fire `on_set_scene`.
    fn set_scene(&mut self, scene: Option<SceneToken>);

    /// `onSetScene` (`scene_provider.cpp:61-64`): nothing by default.
    fn on_set_scene(&mut self, _old: Option<SceneToken>, _new: Option<SceneToken>) {}

    /// `getView(typeID) -> void*` (`scene_provider.cpp:79-83`): the base
    /// answers null.
    fn get_view(&self, type_id: RuntimeTypeID) -> Option<SceneViewRef>;

    /// The provider's tick-listener face — the reference registers
    /// `this` with `TickSceneView::addListener` inside `onSetScene` and
    /// removes it on the null scene; the port hands the face to the scene
    /// (registered adaptation: the token-only unbind cannot reach the old
    /// view, so [`Scene::remove_provider`] performs the removal).
    fn tick_listener(&self) -> Option<Rc<RefCell<dyn ITickSceneListener>>> {
        None
    }

    /// `onSetScene`'s pull side, answered by the scene instead of pulled:
    /// fires after the faces are materialized and before the provider
    /// joins the collection.
    fn on_scene_bound(&mut self, _faces: &SceneBoundFaces) {}

    /// The null-scene half of `onSetScene` (dropping stored faces).
    fn on_scene_unbound(&mut self) {}
}

/// The `SceneProvider::pScene_` face (`scene_provider.hpp:50`) as an
/// embeddable helper: providers forward their trait methods here and hand
/// their `on_set_scene` hook to [`ProviderSceneRef::set_scene`].
#[derive(Default, Debug, Clone, Copy)]
pub struct ProviderSceneRef {
    scene: Option<SceneToken>,
}

impl ProviderSceneRef {
    pub fn scene(&self) -> Option<SceneToken> {
        self.scene
    }

    pub fn set_scene<F: FnOnce(Option<SceneToken>, Option<SceneToken>)>(
        &mut self,
        scene: Option<SceneToken>,
        on_set_scene: F,
    ) {
        let old_scene = self.scene;
        self.scene = scene;
        on_set_scene(old_scene, scene);
    }

    /// Swap the stored scene and answer the previous one — for providers
    /// whose hook needs `&mut self` (the closure form would double-borrow).
    pub fn replace(&mut self, scene: Option<SceneToken>) -> Option<SceneToken> {
        std::mem::replace(&mut self.scene, scene)
    }
}

/// `ISceneView` (`scene_provider.hpp:53-69`): the owner face plus the
/// provider wiring every view implements.
pub trait ISceneView {
    fn scene(&self) -> Option<SceneToken>;
    fn set_scene(&mut self, scene: Option<SceneToken>);
    fn on_set_scene(&mut self, _old: Option<SceneToken>, _new: Option<SceneToken>) {}
    /// `addProvider(SceneProvider*)` (`scene_provider.hpp:80-89`): ask the
    /// provider for this view's interface, collect when supported.
    fn add_scene_provider(&mut self, provider: &Rc<RefCell<dyn SceneProvider>>);
    /// `removeProvider(SceneProvider*)` (`scene_provider.hpp:91-100`).
    fn remove_scene_provider(&mut self, provider: &Rc<RefCell<dyn SceneProvider>>);
}

/// `Scene` (`lib/scene/scene.{hpp,cpp}`): providers in registration order,
/// views and object operations created lazily per type id.
pub struct Scene {
    token: SceneToken,
    providers: Vec<Rc<RefCell<dyn SceneProvider>>>,
    /// The `ViewMap` iteration face (`scene.hpp:105-106`).
    views: Vec<Rc<RefCell<dyn ISceneView>>>,
    /// The typed fetch face of the same views (each entry wraps the
    /// view's `Rc<RefCell<T>>` for downcasting).
    views_typed: HashMap<RuntimeTypeID, Rc<dyn Any>>,
    /// The `ObjectOperationMap` (`scene.hpp:107-108`); entries wrap the
    /// operation's `Rc<RefCell<T>>`.
    object_operations: HashMap<RuntimeTypeID, Rc<dyn Any>>,
}

impl Scene {
    /// `Scene()` — each scene carries its own identity token standing in
    /// for the `this` pointer.
    pub fn new() -> Self {
        Scene {
            token: take_scene_token(),
            providers: Vec::new(),
            views: Vec::new(),
            views_typed: HashMap::new(),
            object_operations: HashMap::new(),
        }
    }

    pub fn token(&self) -> SceneToken {
        self.token
    }

    /// `addProvider` (`scene_provider.cpp` → `scene.cpp:28-47`): the
    /// provider must be new to this scene and currently unbound; it is
    /// bound first, appended, and every existing view is notified.
    ///
    /// The reference's provider pulls its views/operations inside
    /// `onSetScene`; the port materializes the [`SceneBoundFaces`] here —
    /// before the push, so the views' provider walks miss this provider —
    /// and hands them over through `on_scene_bound`.
    pub fn add_provider(&mut self, provider: Rc<RefCell<dyn SceneProvider>>) {
        assert!(
            !self.providers.iter().any(|p| Rc::ptr_eq(p, &provider)),
            "provider added more than once"
        );
        assert!(
            provider.borrow().scene().is_none(),
            "provider is already bound to a scene"
        );

        provider.borrow_mut().set_scene(Some(self.token));

        let faces = SceneBoundFaces {
            tick_view: self.get_view::<TickSceneView>(),
            tick_operation: self.get_object_operation::<TickOperation>(),
            collision_operation: self.get_object_operation::<CollisionOperation>(),
            object_change_view: self.get_view::<ObjectChangeSceneView>(),
        };
        provider.borrow_mut().on_scene_bound(&faces);

        // The tick-listener registration the reference performs inside
        // onSetScene (`pTickScene->addListener(this)`).
        let listener = provider.borrow().tick_listener();
        if let Some(listener) = listener {
            faces.tick_view.borrow_mut().add_listener(listener);
        }

        self.providers.push(provider.clone());

        // Notify every existing scene interface.
        for view in &self.views {
            view.borrow_mut().add_scene_provider(&provider);
        }
    }

    /// `removeProvider` (`scene.cpp:50-71`): views first, then the
    /// collection, then the tick-listener face (the port-scene stand-in
    /// for the provider's own `removeListener`), then the unbind.
    pub fn remove_provider(&mut self, provider: &Rc<RefCell<dyn SceneProvider>>) {
        for view in &self.views {
            view.borrow_mut().remove_scene_provider(provider);
        }

        self.providers.retain(|p| !Rc::ptr_eq(p, provider));

        let listener = provider.borrow().tick_listener();
        if let Some(listener) = listener {
            let tick_view = self.get_view::<TickSceneView>();
            tick_view.borrow_mut().remove_listener(&listener);
        }

        let current = provider.borrow().scene();
        assert!(current == Some(self.token) || current.is_none());
        provider.borrow_mut().set_scene(None);
        provider.borrow_mut().on_scene_unbound();
    }

    /// `getView<InterfaceType>` (`scene.hpp:23-41` + `createView` at
    /// 81-96): the first ask creates the view, binds it to this scene and
    /// walks every registered provider; later asks answer the same view.
    pub fn get_view<T: ISceneView + Default + 'static>(&mut self) -> Rc<RefCell<T>> {
        let type_id = scene_view_runtime_id::<T>();
        if let Some(existing) = self.views_typed.get(&type_id) {
            let wrapped = existing
                .clone()
                .downcast::<Rc<RefCell<T>>>()
                .expect("stored view kind must match");
            return (*wrapped).clone();
        }

        let view: Rc<RefCell<T>> = Rc::new(RefCell::new(T::default()));
        view.borrow_mut().set_scene(Some(self.token));

        let as_view: Rc<RefCell<dyn ISceneView>> = view.clone();
        for provider in &self.providers {
            as_view.borrow_mut().add_scene_provider(provider);
        }

        self.views.push(as_view);
        self.views_typed.insert(type_id, Rc::new(view.clone()));
        view
    }

    /// `getObjectOperation<ObjectOperationType>` (`scene.hpp:49-70` +
    /// `createObjectInterface` at 98-103): created bare, with no wiring.
    pub fn get_object_operation<T: Default + 'static>(&mut self) -> Rc<RefCell<T>> {
        let type_id = object_operation_runtime_id::<T>();
        let existing = self
            .object_operations
            .entry(type_id)
            .or_insert_with(|| Rc::new(Rc::new(RefCell::new(T::default()))) as Rc<dyn Any>);
        let wrapped = existing
            .clone()
            .downcast::<Rc<RefCell<T>>>()
            .expect("stored operation kind must match");
        (*wrapped).clone()
    }
}

impl Default for Scene {
    fn default() -> Self {
        Self::new()
    }
}

/// `impl ISceneView for TickSceneView` — the trait delegates to the view's
/// inherent methods; `add/remove_scene_provider` implement the
/// `SceneView<ITickSceneProvider>` ask-and-collect face
/// (`scene_provider.hpp:80-100`).
impl ISceneView for TickSceneView {
    fn scene(&self) -> Option<SceneToken> {
        TickSceneView::scene(self)
    }

    fn set_scene(&mut self, scene: Option<SceneToken>) {
        TickSceneView::set_scene(self, scene);
    }

    fn add_scene_provider(&mut self, provider: &Rc<RefCell<dyn SceneProvider>>) {
        let asked = provider.borrow().get_view(tick_provider_view_id());
        if let Some(SceneViewRef::Tick(face)) = asked {
            self.add_provider(face);
        }
    }

    fn remove_scene_provider(&mut self, provider: &Rc<RefCell<dyn SceneProvider>>) {
        let asked = provider.borrow().get_view(tick_provider_view_id());
        if let Some(SceneViewRef::Tick(face)) = asked {
            self.remove_provider(&face);
        }
    }
}

/// The collision view's ask-and-collect face.
impl ISceneView for CollisionSceneView {
    fn scene(&self) -> Option<SceneToken> {
        CollisionSceneView::scene(self)
    }

    fn set_scene(&mut self, scene: Option<SceneToken>) {
        CollisionSceneView::set_scene(self, scene);
    }

    fn add_scene_provider(&mut self, provider: &Rc<RefCell<dyn SceneProvider>>) {
        let asked = provider.borrow().get_view(collision_provider_view_id());
        if let Some(SceneViewRef::Collision(face)) = asked {
            self.add_provider(face);
        }
    }

    fn remove_scene_provider(&mut self, provider: &Rc<RefCell<dyn SceneProvider>>) {
        let asked = provider.borrow().get_view(collision_provider_view_id());
        if let Some(SceneViewRef::Collision(face)) = asked {
            self.remove_provider(&face);
        }
    }
}

/// The intersect view's ask-and-collect face.
impl ISceneView for IntersectSceneView {
    fn scene(&self) -> Option<SceneToken> {
        IntersectSceneView::scene(self)
    }

    fn set_scene(&mut self, scene: Option<SceneToken>) {
        IntersectSceneView::set_scene(self, scene);
    }

    fn add_scene_provider(&mut self, provider: &Rc<RefCell<dyn SceneProvider>>) {
        let asked = provider.borrow().get_view(intersect_provider_view_id());
        if let Some(SceneViewRef::Intersect(face)) = asked {
            self.add_provider(face);
        }
    }

    fn remove_scene_provider(&mut self, provider: &Rc<RefCell<dyn SceneProvider>>) {
        let asked = provider.borrow().get_view(intersect_provider_view_id());
        if let Some(SceneViewRef::Intersect(face)) = asked {
            self.remove_provider(&face);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntersectionSet;
    use crate::scene_intersect_context::SceneIntersectContext;
    use crate::tick_view::TickOperation;
    use foundation::math::{ConvexHull, Vector3};
    use physics::{CollisionState, SweepParams, WorldTriangle};
    use std::cell::Cell;

    struct TickFace {
        ticks: Cell<u32>,
    }

    impl ITickSceneProvider for TickFace {
        fn tick(&self, _d_time: f32) {
            self.ticks.set(self.ticks.get() + 1);
        }
        fn update_animations(&self, _d_time: f32) {}
    }

    /// A provider that supports the tick view, keeping its scene binding
    /// in a `ProviderSceneRef`.
    struct TickProvider {
        scene_ref: ProviderSceneRef,
        tick_face: Rc<TickFace>,
        on_set_scene_events: RefCell<Vec<Option<SceneToken>>>,
    }

    impl TickProvider {
        fn new() -> Rc<RefCell<Self>> {
            Rc::new(RefCell::new(Self {
                scene_ref: ProviderSceneRef::default(),
                tick_face: Rc::new(TickFace {
                    ticks: Cell::new(0),
                }),
                on_set_scene_events: RefCell::new(Vec::new()),
            }))
        }
    }

    impl SceneProvider for TickProvider {
        fn scene(&self) -> Option<SceneToken> {
            self.scene_ref.scene()
        }

        fn set_scene(&mut self, scene: Option<SceneToken>) {
            let old = self.scene_ref.replace(scene);
            self.on_set_scene(old, scene);
        }

        fn on_set_scene(&mut self, _old: Option<SceneToken>, new: Option<SceneToken>) {
            self.on_set_scene_events.borrow_mut().push(new);
        }

        fn get_view(&self, type_id: RuntimeTypeID) -> Option<SceneViewRef> {
            let mut out = None;
            expose_tick_view(self.tick_face.clone(), type_id, &mut out);
            out
        }
    }

    /// A provider that supports no view at all (base `getView` null).
    struct MuteProvider {
        scene_ref: ProviderSceneRef,
    }

    impl MuteProvider {
        fn new() -> Rc<RefCell<Self>> {
            Rc::new(RefCell::new(Self {
                scene_ref: ProviderSceneRef::default(),
            }))
        }
    }

    impl SceneProvider for MuteProvider {
        fn scene(&self) -> Option<SceneToken> {
            self.scene_ref.scene()
        }

        fn set_scene(&mut self, scene: Option<SceneToken>) {
            self.scene_ref.replace(scene);
        }

        fn get_view(&self, _type_id: RuntimeTypeID) -> Option<SceneViewRef> {
            None
        }
    }

    fn as_provider(p: &Rc<RefCell<TickProvider>>) -> Rc<RefCell<dyn SceneProvider>> {
        p.clone()
    }

    fn as_mute(p: &Rc<RefCell<MuteProvider>>) -> Rc<RefCell<dyn SceneProvider>> {
        p.clone()
    }

    #[test]
    fn provider_scene_ref_fires_the_hook_with_old_and_new() {
        let mut scene_ref = ProviderSceneRef::default();
        assert!(scene_ref.scene().is_none());

        let mut events = Vec::new();
        scene_ref.set_scene(Some(7), |old, new| events.push((old, new)));
        assert_eq!(scene_ref.scene(), Some(7));
        scene_ref.set_scene(None, |old, new| events.push((old, new)));
        assert_eq!(events, [(None, Some(7)), (Some(7), None)]);
    }

    #[test]
    fn adding_a_provider_binds_it_and_wires_existing_views() {
        let mut scene = Scene::new();
        let provider = TickProvider::new();

        // View created before the provider exists: it starts empty...
        let view = scene.get_view::<TickSceneView>();
        assert_eq!(view.borrow().providers_len(), 0);
        assert_eq!(scene.providers.len(), 0);

        scene.add_provider(as_provider(&provider));

        // ...then the add notifies the existing view.
        assert_eq!(view.borrow().providers_len(), 1);
        // The provider is bound and saw its hook fire with the new token.
        assert_eq!(provider.borrow().scene(), Some(scene.token()));
        assert_eq!(
            *provider.borrow().on_set_scene_events.borrow(),
            [Some(scene.token())]
        );

        // The view drives the provider.
        view.borrow_mut().tick(0.1);
        assert_eq!(provider.borrow().tick_face.ticks.get(), 1);
    }

    #[test]
    fn views_created_after_the_providers_walk_them_all() {
        let mut scene = Scene::new();
        let tick = TickProvider::new();
        let mute = MuteProvider::new();

        scene.add_provider(as_provider(&tick));
        scene.add_provider(as_mute(&mute));

        let view = scene.get_view::<TickSceneView>();
        assert_eq!(
            view.borrow().providers_len(),
            1,
            "the mute provider exposes no tick face and is skipped"
        );

        // getView answers the same view on later asks.
        let again = scene.get_view::<TickSceneView>();
        assert!(Rc::ptr_eq(&view, &again));
    }

    #[test]
    fn remove_provider_unwires_the_views_then_unbinds() {
        let mut scene = Scene::new();
        let provider = TickProvider::new();
        scene.add_provider(as_provider(&provider));

        let view = scene.get_view::<TickSceneView>();
        assert_eq!(view.borrow().providers_len(), 1);

        scene.remove_provider(&as_provider(&provider));
        assert_eq!(view.borrow().providers_len(), 0);
        assert!(provider.borrow().scene().is_none());
        assert!(scene.providers.is_empty());
    }

    #[test]
    #[should_panic(expected = "provider added more than once")]
    fn adding_the_same_provider_twice_asserts() {
        let mut scene = Scene::new();
        let provider = TickProvider::new();
        scene.add_provider(as_provider(&provider));
        scene.add_provider(as_provider(&provider));
    }

    #[test]
    #[should_panic(expected = "already bound to a scene")]
    fn adding_a_bound_provider_asserts() {
        let mut first = Scene::new();
        let mut second = Scene::new();
        let provider = TickProvider::new();
        first.add_provider(as_provider(&provider));
        second.add_provider(as_provider(&provider));
    }

    #[test]
    fn object_operations_are_lazy_singletons_per_type() {
        let mut scene = Scene::new();
        let first = scene.get_object_operation::<TickOperation>();
        let second = scene.get_object_operation::<TickOperation>();
        assert!(Rc::ptr_eq(&first, &second));
    }

    #[test]
    fn erased_faces_default_hooks_and_misses() {
        // Scene::default carries its own non-zero token.
        let mut scene = Scene::default();
        assert_ne!(scene.token(), 0);

        // exposeView rejects other interface ids and keeps a filled out.
        let face: Rc<dyn ITickSceneProvider> = Rc::new(TickFace {
            ticks: Cell::new(0),
        });
        let mut out = None;
        expose_tick_view(face, tick_provider_view_id() + 1, &mut out);
        assert!(out.is_none(), "a different interface id gets nothing");
        expose_tick_view(
            Rc::new(TickFace {
                ticks: Cell::new(0),
            }),
            tick_provider_view_id(),
            &mut out,
        );
        assert!(matches!(out, Some(SceneViewRef::Tick(_))));
        expose_tick_view(
            Rc::new(TickFace {
                ticks: Cell::new(0),
            }),
            tick_provider_view_id(),
            &mut out,
        );
        assert!(
            matches!(out, Some(SceneViewRef::Tick(_))),
            "an already-filled out stays untouched"
        );

        // The provider-side onSetScene default is a no-op.
        let mute = MuteProvider::new();
        as_mute(&mute).borrow_mut().on_set_scene(None, None);

        // A provider answering no tick face exercises the collect-nothing
        // branch of both view wiring directions.
        let tick = TickProvider::new();
        scene.add_provider(as_provider(&tick));
        scene.add_provider(as_mute(&mute));
        let view = scene.get_view::<TickSceneView>();
        assert_eq!(view.borrow().providers_len(), 1);
        view.borrow_mut().update_animations(0.3);
        scene.remove_provider(&as_mute(&mute));
        assert_eq!(view.borrow().providers_len(), 1);

        // The erased view faces: scene() reads through and onSetScene hits
        // the trait default (TickSceneView does not override it).
        let erased = scene.views[0].clone();
        assert_eq!(erased.borrow().scene(), Some(scene.token()));
        erased.borrow_mut().on_set_scene(None, None);
    }

    /// A provider supporting the tick face plus both query faces and a
    /// tick listener — the dynamic-scene-provider shape.
    struct CollisionFace {
        stops: Cell<bool>,
    }

    impl ICollisionSceneViewProvider for CollisionFace {
        fn collide_ray(
            &self,
            _source: &Vector3,
            _extent: &Vector3,
            _sp: &SweepParams,
            _state: &mut CollisionState,
        ) -> bool {
            self.stops.get()
        }

        fn collide_prism(
            &self,
            _source: &WorldTriangle,
            _extent: &Vector3,
            _sp: &SweepParams,
            _state: &mut CollisionState,
        ) -> bool {
            false
        }
    }

    struct IntersectFace;

    impl IIntersectSceneViewProvider for IntersectFace {
        fn intersect(
            &self,
            _context: &SceneIntersectContext,
            _hull: &ConvexHull,
            _intersection: &mut IntersectionSet,
        ) -> usize {
            1
        }
    }

    #[derive(Default)]
    struct ListenerFace {
        events: RefCell<Vec<&'static str>>,
    }

    impl ITickSceneListener for ListenerFace {
        fn on_pre_tick(&mut self) {
            self.events.borrow_mut().push("pre");
        }
        fn on_post_tick(&mut self) {
            self.events.borrow_mut().push("post");
        }
    }

    /// A provider exposing all three faces plus a tick listener, logging
    /// its bind/unbind hooks.
    struct FullProvider {
        scene_ref: ProviderSceneRef,
        tick_face: Rc<TickFace>,
        collision_face: Rc<CollisionFace>,
        intersect_face: Rc<IntersectFace>,
        listener: Rc<RefCell<ListenerFace>>,
        hooks: RefCell<Vec<&'static str>>,
    }

    impl FullProvider {
        fn new() -> Rc<RefCell<Self>> {
            Rc::new(RefCell::new(Self {
                scene_ref: ProviderSceneRef::default(),
                tick_face: Rc::new(TickFace {
                    ticks: Cell::new(0),
                }),
                collision_face: Rc::new(CollisionFace {
                    stops: Cell::new(false),
                }),
                intersect_face: Rc::new(IntersectFace),
                listener: Rc::new(RefCell::new(ListenerFace::default())),
                hooks: RefCell::new(Vec::new()),
            }))
        }
    }

    impl SceneProvider for FullProvider {
        fn scene(&self) -> Option<SceneToken> {
            self.scene_ref.scene()
        }

        fn set_scene(&mut self, scene: Option<SceneToken>) {
            let old = self.scene_ref.replace(scene);
            self.on_set_scene(old, scene);
        }

        fn get_view(&self, type_id: RuntimeTypeID) -> Option<SceneViewRef> {
            let mut out = None;
            expose_tick_view(self.tick_face.clone(), type_id, &mut out);
            expose_collision_view(self.collision_face.clone(), type_id, &mut out);
            expose_intersect_view(self.intersect_face.clone(), type_id, &mut out);
            out
        }

        fn tick_listener(&self) -> Option<Rc<RefCell<dyn ITickSceneListener>>> {
            Some(self.listener.clone())
        }

        fn on_scene_bound(&mut self, _faces: &SceneBoundFaces) {
            self.hooks.borrow_mut().push("bound");
        }

        fn on_scene_unbound(&mut self) {
            self.hooks.borrow_mut().push("unbound");
        }
    }

    fn as_full(p: &Rc<RefCell<FullProvider>>) -> Rc<RefCell<dyn SceneProvider>> {
        p.clone()
    }

    #[test]
    fn the_query_views_and_listener_wire_through_the_scene() {
        let mut scene = Scene::new();
        let provider = FullProvider::new();
        let mute = MuteProvider::new();

        scene.add_provider(as_full(&provider));
        scene.add_provider(as_mute(&mute));
        assert_eq!(
            *provider.borrow().hooks.borrow(),
            ["bound"],
            "on_scene_bound fired at bind"
        );

        // The listener joined the tick view and frames the provider tick.
        let tick_view = scene.get_view::<TickSceneView>();
        tick_view.borrow_mut().tick(0.2);
        assert_eq!(
            *provider.borrow().listener.borrow().events.borrow(),
            ["pre", "post"]
        );

        // Both query views collect the provider face: these views are not
        // part of the bound faces, so their creation still walks the
        // registered providers.
        let collision_view = scene.get_view::<CollisionSceneView>();
        assert_eq!(collision_view.borrow().providers_len(), 1);
        let intersect_view = scene.get_view::<IntersectSceneView>();
        assert_eq!(intersect_view.borrow().providers_len(), 1);

        struct NopCallback;
        impl physics::CollisionCallback for NopCallback {}
        let dist = collision_view.borrow().collide_ray(
            &Vector3::ZERO,
            &Vector3::new(0.0, 0.0, 10.0),
            &mut NopCallback,
        );
        assert_eq!(dist, -1.0, "a passing face leaves the initial dist");

        // A stopping face ends the view walk through the trait wiring.
        provider.borrow().collision_face.stops.set(true);
        let dist = collision_view.borrow().collide_ray(
            &Vector3::ZERO,
            &Vector3::new(0.0, 0.0, 10.0),
            &mut NopCallback,
        );
        assert_eq!(dist, -1.0, "the face stops without touching the state");

        // The prism and intersect faces run through their views too.
        let triangle = WorldTriangle::new(
            Vector3::new(-1.0, -1.0, 0.0),
            Vector3::new(1.0, -1.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            0,
        );
        let prism_dist = collision_view.borrow().collide_prism(
            &triangle,
            &Vector3::new(0.0, 0.0, 4.0),
            &mut NopCallback,
        );
        assert_eq!(prism_dist, -1.0);
        let hull = ConvexHull::new(vec![]);
        let context = SceneIntersectContext::new(None);
        let mut intersection = IntersectionSet::new();
        let total = intersect_view
            .borrow()
            .intersect(&context, &hull, &mut intersection);
        assert_eq!(total, 1, "the intersect face answered through the view");

        // The erased view faces route through the trait impls.
        let as_erased: Rc<RefCell<dyn ISceneView>> = collision_view.clone();
        assert_eq!(as_erased.borrow().scene(), Some(scene.token()));
        as_erased.borrow_mut().set_scene(None);
        as_erased.borrow_mut().on_set_scene(None, None);
        let intersect_erased: Rc<RefCell<dyn ISceneView>> = intersect_view.clone();
        assert_eq!(intersect_erased.borrow().scene(), Some(scene.token()));

        // The expose faces: a mismatched id fills nothing, a match fills,
        // and an already-filled out stays untouched.
        let face: Rc<dyn ICollisionSceneViewProvider> = Rc::new(CollisionFace {
            stops: Cell::new(false),
        });
        let mut out = None;
        expose_collision_view(face, tick_provider_view_id(), &mut out);
        assert!(out.is_none(), "a different interface id gets nothing");
        expose_collision_view(
            Rc::new(CollisionFace {
                stops: Cell::new(false),
            }),
            collision_provider_view_id(),
            &mut out,
        );
        assert!(matches!(out, Some(SceneViewRef::Collision(_))));
        expose_collision_view(
            Rc::new(CollisionFace {
                stops: Cell::new(false),
            }),
            collision_provider_view_id(),
            &mut out,
        );
        assert!(
            matches!(out, Some(SceneViewRef::Collision(_))),
            "an already-filled out stays untouched"
        );

        let hull_face: Rc<dyn IIntersectSceneViewProvider> = Rc::new(IntersectFace);
        let mut hull_out = None;
        expose_intersect_view(
            hull_face.clone(),
            collision_provider_view_id(),
            &mut hull_out,
        );
        assert!(hull_out.is_none());
        expose_intersect_view(hull_face, intersect_provider_view_id(), &mut hull_out);
        assert!(matches!(hull_out, Some(SceneViewRef::Intersect(_))));

        // Unbind: the views lose the face, the listener leaves the tick
        // view and on_scene_unbound fires.
        scene.remove_provider(&as_full(&provider));
        scene.remove_provider(&as_mute(&mute));
        assert_eq!(
            *provider.borrow().hooks.borrow(),
            ["bound", "unbound"],
            "on_scene_unbound fired at unbind"
        );
        assert_eq!(collision_view.borrow().providers_len(), 0);
        assert_eq!(intersect_view.borrow().providers_len(), 0);
        tick_view.borrow_mut().tick(0.2);
        assert_eq!(
            *provider.borrow().listener.borrow().events.borrow(),
            ["pre", "post"],
            "the listener left the tick view"
        );
    }
}
