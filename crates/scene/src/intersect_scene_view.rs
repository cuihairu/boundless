//! The intersect view (`lib/scene/intersect_scene_view.{hpp,cpp}`): the
//! visibility projection collecting `IIntersectSceneViewProvider` faces
//! and summing their per-provider object counts into one `IntersectionSet`.
//!
//! `cull` builds a frustum from the world-view-projection matrix and hands
//! its hull to `intersect` (`intersect_scene_view.cpp:33-37`); the frustum
//! machinery (`lib/math/frustum_hull.*`) is not ported in this slice, so
//! `cull` is not ported either (registered boundary).

use std::rc::Rc;

use crate::SceneToken;
use crate::intersection_set::IntersectionSet;
use crate::scene_intersect_context::SceneIntersectContext;
use foundation::math::ConvexHull;

/// `IIntersectSceneViewProvider` (`intersect_scene_view.hpp:22-31`): the
/// answered count is the provider's contribution to `totalVisible`.
pub trait IIntersectSceneViewProvider {
    fn intersect(
        &self,
        context: &SceneIntersectContext,
        hull: &ConvexHull,
        intersection: &mut IntersectionSet,
    ) -> usize;
}

/// `IntersectSceneView : SceneView<IIntersectSceneViewProvider>`
/// (`intersect_scene_view.hpp:33-42`).
#[derive(Default)]
pub struct IntersectSceneView {
    /// `ISceneView::pOwner_`.
    p_owner: Option<SceneToken>,
    providers: Vec<Rc<dyn IIntersectSceneViewProvider>>,
}

impl IntersectSceneView {
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

    /// `SceneView::addProvider` (`scene_provider.hpp:102-105`).
    pub fn add_provider(&mut self, provider: Rc<dyn IIntersectSceneViewProvider>) {
        self.providers.push(provider);
    }

    pub fn providers_len(&self) -> usize {
        self.providers.len()
    }

    /// `removeProvider` erases by pointer identity
    /// (`scene_provider.hpp:107-115`).
    pub fn remove_provider(&mut self, provider: &Rc<dyn IIntersectSceneViewProvider>) {
        self.providers.retain(|p| !Rc::ptr_eq(p, provider));
    }

    /// `intersect` (`intersect_scene_view.cpp:14-31`): every provider runs,
    /// the answered counts sum.
    pub fn intersect(
        &self,
        context: &SceneIntersectContext,
        hull: &ConvexHull,
        intersection: &mut IntersectionSet,
    ) -> usize {
        let mut total_visible = 0usize;
        for provider in &self.providers {
            total_visible += provider.intersect(context, hull, intersection);
        }

        total_visible
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_object::SceneObject;
    use crate::scene_object_flags::SceneObjectFlags;
    use foundation::math::{PlaneEq, Vector3};

    struct CountingProvider {
        objects: Vec<SceneObject>,
    }

    impl IIntersectSceneViewProvider for CountingProvider {
        fn intersect(
            &self,
            _context: &SceneIntersectContext,
            _hull: &ConvexHull,
            intersection: &mut IntersectionSet,
        ) -> usize {
            for object in &self.objects {
                intersection.insert(object.clone());
            }
            self.objects.len()
        }
    }

    #[test]
    fn the_view_sums_provider_counts_into_one_intersection_set() {
        let mut view = IntersectSceneView::new();
        assert!(view.scene().is_none());
        view.set_scene(Some(5));
        assert_eq!(view.scene(), Some(5));

        let unit_type = SceneObject::type_of::<u32>();
        let first = Rc::new(CountingProvider {
            objects: vec![
                SceneObject::typed::<u32>(1, SceneObjectFlags::new()),
                SceneObject::typed::<u32>(2, SceneObjectFlags::new()),
            ],
        });
        let second = Rc::new(CountingProvider {
            objects: vec![SceneObject::typed::<u64>(3, SceneObjectFlags::new())],
        });
        view.add_provider(first.clone());
        view.add_provider(second);
        assert_eq!(view.providers_len(), 2);

        let hull = ConvexHull::new(vec![PlaneEq::new(Vector3::new(0.0, 0.0, 1.0), 0.0)]);
        let context = SceneIntersectContext::new(Some(5));
        let mut intersection = IntersectionSet::new();
        let total = view.intersect(&context, &hull, &mut intersection);
        assert_eq!(total, 3, "both providers' counts sum");
        assert_eq!(intersection.num_objects(), 3);
        assert_eq!(intersection.objects_of_type(unit_type).len(), 2);

        // Removal by identity drops exactly that face; an unknown face
        // removes nothing.
        let unknown: Rc<dyn IIntersectSceneViewProvider> = Rc::new(CountingProvider {
            objects: Vec::new(),
        });
        view.remove_provider(&unknown);
        assert_eq!(view.providers_len(), 2);
        let as_face: Rc<dyn IIntersectSceneViewProvider> = first.clone();
        view.remove_provider(&as_face);
        assert_eq!(view.providers_len(), 1);
    }
}
