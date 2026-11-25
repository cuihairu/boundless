//! The object-change event view (`lib/scene/scene_event_view.{hpp,cpp}` +
//! `object_change_scene_view.{hpp,cpp}`): providers report added/removed/
//! changed objects and registered delegates receive them.
//!
//! The reference builds on `cstdmf/event.hpp` (`Event<Scene, Args>` with a
//! delegate list); the port keeps the same three named lists with a plain
//! delegate `Vec` invoked with `(scene(), args)`. `SceneEventView`'s
//! add/removeProvider are deliberate no-ops ("Ignore all providers in an
//! event view", `scene_event_view.cpp:9-17`).

use crate::SceneToken;
use crate::scene_object::SceneObject;
use std::rc::Rc;

/// `ObjectChangeSceneView::BaseObjectsEventArgs` (`object_change_scene_view.hpp:19-24`):
/// the announcing provider and the object batch. The reference carries a
/// `SceneProvider*`; the port carries the provider's registered identity
/// token.
pub struct ObjectChangeArgs<'a> {
    pub provider: SceneToken,
    pub objects: &'a [SceneObject],
}

/// `Event<Scene, const Objects*EventArgs&>::EventDelegate` — the port's
/// delegate face. The view holds delegates behind shared ownership, so the
/// callback takes `&self` and any recording goes through interior
/// mutability.
pub trait ObjectChangeDelegate {
    fn on_objects(&self, scene: Option<SceneToken>, args: &ObjectChangeArgs);
}

/// `ObjectChangeSceneView` (`object_change_scene_view.hpp:31-47`).
#[derive(Default)]
pub struct ObjectChangeSceneView {
    /// `ISceneView::pOwner_`.
    p_owner: Option<SceneToken>,
    objects_added: Vec<Box<dyn ObjectChangeDelegate>>,
    objects_removed: Vec<Box<dyn ObjectChangeDelegate>>,
    objects_changed: Vec<Box<dyn ObjectChangeDelegate>>,
}

impl ObjectChangeSceneView {
    pub fn new() -> Self {
        Self::default()
    }

    /// `ISceneView::scene()` (`scene_provider.cpp:27-30`).
    pub fn scene(&self) -> Option<SceneToken> {
        self.p_owner
    }

    /// `ISceneView::scene(pScene)` (`scene_provider.cpp:18-24`).
    pub fn set_scene(&mut self, scene: Option<SceneToken>) {
        let old_scene = self.p_owner;
        self.p_owner = scene;
        self.on_set_scene(old_scene, scene);
    }

    /// `ISceneView::onSetScene` (`scene_provider.cpp:33-36`).
    pub fn on_set_scene(&mut self, _old_scene: Option<SceneToken>, _new_scene: Option<SceneToken>) {
    }

    /// `objectsAdded()` (`object_change_scene_view.cpp:11-16`): the
    /// delegate list itself.
    pub fn objects_added(&mut self) -> &mut Vec<Box<dyn ObjectChangeDelegate>> {
        &mut self.objects_added
    }

    /// `objectsRemoved()` (`object_change_scene_view.cpp:18-23`).
    pub fn objects_removed(&mut self) -> &mut Vec<Box<dyn ObjectChangeDelegate>> {
        &mut self.objects_removed
    }

    /// `objectsChanged()` (`object_change_scene_view.cpp:25-30`).
    pub fn objects_changed(&mut self) -> &mut Vec<Box<dyn ObjectChangeDelegate>> {
        &mut self.objects_changed
    }

    /// `notifyObjectsAdded` (`object_change_scene_view.cpp:32-41`).
    pub fn notify_objects_added(&self, provider: SceneToken, objects: &[SceneObject]) {
        let args = ObjectChangeArgs { provider, objects };
        for delegate in &self.objects_added {
            delegate.on_objects(self.p_owner, &args);
        }
    }

    /// `notifyObjectsRemoved` (`object_change_scene_view.cpp:43-52`).
    pub fn notify_objects_removed(&self, provider: SceneToken, objects: &[SceneObject]) {
        let args = ObjectChangeArgs { provider, objects };
        for delegate in &self.objects_removed {
            delegate.on_objects(self.p_owner, &args);
        }
    }

    /// `notifyObjectsChanged` (`object_change_scene_view.cpp:54-63`).
    pub fn notify_objects_changed(&self, provider: SceneToken, objects: &[SceneObject]) {
        let args = ObjectChangeArgs { provider, objects };
        for delegate in &self.objects_changed {
            delegate.on_objects(self.p_owner, &args);
        }
    }
}

/// `ISceneView` face for the event view. `SceneEventView` overrides
/// add/removeProvider to ignore providers (`scene_event_view.cpp:9-17`);
/// `scene`/`set_scene` follow the base.
impl crate::ISceneView for ObjectChangeSceneView {
    fn scene(&self) -> Option<SceneToken> {
        ObjectChangeSceneView::scene(self)
    }

    fn set_scene(&mut self, scene: Option<SceneToken>) {
        ObjectChangeSceneView::set_scene(self, scene);
    }

    fn add_scene_provider(&mut self, _provider: &Rc<std::cell::RefCell<dyn crate::SceneProvider>>) {
        // Ignore all providers in an event view.
    }

    fn remove_scene_provider(
        &mut self,
        _provider: &Rc<std::cell::RefCell<dyn crate::SceneProvider>>,
    ) {
        // Ignore all providers in an event view.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_object_flags::SceneObjectFlags;
    use crate::{
        ProviderSceneRef, RuntimeTypeID, SceneProvider, SceneViewRef, tick_provider_view_id,
    };
    use std::cell::RefCell;

    /// A provider supporting no view at all (base `getView` null).
    #[derive(Default)]
    struct AnyProvider {
        scene_ref: ProviderSceneRef,
    }

    impl SceneProvider for AnyProvider {
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

    type CallLog = Vec<(Option<SceneToken>, SceneToken, Vec<u64>)>;

    struct Recorder {
        calls: Rc<RefCell<CallLog>>,
    }

    impl ObjectChangeDelegate for Recorder {
        fn on_objects(&self, scene: Option<SceneToken>, args: &ObjectChangeArgs) {
            self.calls.borrow_mut().push((
                scene,
                args.provider,
                args.objects.iter().map(SceneObject::handle).collect(),
            ));
        }
    }

    fn object(handle: u64) -> SceneObject {
        SceneObject::typed::<u32>(handle, SceneObjectFlags::new())
    }

    #[test]
    fn notifications_reach_the_registered_delegate_with_the_scene_face() {
        let mut view = ObjectChangeSceneView::new();
        assert!(view.scene().is_none());
        view.set_scene(Some(11));
        assert_eq!(view.scene(), Some(11));

        let added_log: Rc<RefCell<CallLog>> = Rc::new(RefCell::new(Vec::new()));
        let removed_log: Rc<RefCell<CallLog>> = Rc::new(RefCell::new(Vec::new()));
        let changed_log: Rc<RefCell<CallLog>> = Rc::new(RefCell::new(Vec::new()));
        view.objects_added().push(Box::new(Recorder {
            calls: added_log.clone(),
        }));
        view.objects_removed().push(Box::new(Recorder {
            calls: removed_log.clone(),
        }));
        view.objects_changed().push(Box::new(Recorder {
            calls: changed_log.clone(),
        }));

        let batch = [object(1), object(2)];
        view.notify_objects_added(42, &batch);
        view.notify_objects_removed(42, &batch);
        view.notify_objects_changed(42, &batch);

        for log in [&added_log, &removed_log, &changed_log] {
            assert_eq!(
                *log.borrow(),
                [(Some(11), 42, vec![1, 2])],
                "each list got its own notification"
            );
        }

        // The erased ISceneView face: scene()/set_scene delegate to the
        // inherent faces, onSetScene keeps the default no-op.
        let erased: Rc<RefCell<dyn crate::ISceneView>> = Rc::new(RefCell::new(view));
        erased.borrow_mut().set_scene(None);
        assert_eq!(erased.borrow().scene(), None);
        erased.borrow_mut().on_set_scene(None, None);

        // The provider face answers nothing (the base getView null) and
        // the base scene face round-trips.
        let provider = Rc::new(RefCell::new(AnyProvider::default()));
        let as_provider: Rc<RefCell<dyn SceneProvider>> = provider.clone();
        assert!(as_provider.borrow().scene().is_none());
        as_provider.borrow_mut().set_scene(Some(3));
        assert_eq!(as_provider.borrow().scene(), Some(3));
        assert!(
            as_provider
                .borrow()
                .get_view(tick_provider_view_id())
                .is_none()
        );

        // The event view ignores every provider the scene offers it
        // (`SceneEventView` no-ops).
        erased.borrow_mut().add_scene_provider(&as_provider);
        erased.borrow_mut().remove_scene_provider(&as_provider);
        assert_eq!(erased.borrow().scene(), None);
    }
}
