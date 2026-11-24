//! The collision view (`lib/scene/collision_scene_view.{hpp,cpp}`): the
//! query projection collecting `ICollisionSceneViewProvider` faces and
//! sweeping through them until one stops the sweep.
//!
//! The reference walks `providers_` and returns `cs.dist_` as soon as a
//! provider answers true (`collision_scene_view.cpp:15-38`); the sweep
//! plumbing (`SweepShape`/`SweepParams`/`CollisionState`) is shared with
//! the physics port verbatim.

use std::rc::Rc;

use crate::SceneToken;
use foundation::math::Vector3;
use physics::{CollisionCallback, CollisionState, SweepParams, SweepShape, WorldTriangle};

/// `ICollisionSceneViewProvider` (`collision_scene_view.hpp:24-35`).
pub trait ICollisionSceneViewProvider {
    fn collide_ray(
        &self,
        source: &Vector3,
        extent: &Vector3,
        sp: &SweepParams,
        state: &mut CollisionState,
    ) -> bool;

    fn collide_prism(
        &self,
        source: &WorldTriangle,
        extent: &Vector3,
        sp: &SweepParams,
        state: &mut CollisionState,
    ) -> bool;
}

/// `CollisionSceneView : SceneView<ICollisionSceneViewProvider>`
/// (`collision_scene_view.hpp:37-46`).
#[derive(Default)]
pub struct CollisionSceneView {
    /// `ISceneView::pOwner_`.
    p_owner: Option<SceneToken>,
    providers: Vec<Rc<dyn ICollisionSceneViewProvider>>,
}

impl CollisionSceneView {
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
    pub fn add_provider(&mut self, provider: Rc<dyn ICollisionSceneViewProvider>) {
        self.providers.push(provider);
    }

    pub fn providers_len(&self) -> usize {
        self.providers.len()
    }

    /// `removeProvider` erases by pointer identity
    /// (`scene_provider.hpp:107-115`).
    pub fn remove_provider(&mut self, provider: &Rc<dyn ICollisionSceneViewProvider>) {
        self.providers.retain(|p| !Rc::ptr_eq(p, provider));
    }

    /// `collide(start, end, cc)` (`collision_scene_view.cpp:15-32`): the
    /// ray sweep; the first provider that stops it ends the walk.
    pub fn collide_ray(
        &self,
        start: &Vector3,
        end: &Vector3,
        cc: &mut dyn CollisionCallback,
    ) -> f32 {
        let shape = SweepShape::Ray(*start);
        let sp = SweepParams::new(&shape, *end);
        let mut cs = CollisionState::new(cc);

        for provider in &self.providers {
            if provider.collide_ray(start, end, &sp, &mut cs) {
                return cs.dist;
            }
        }

        cs.dist
    }

    /// `collide(start, end, cc)` (`collision_scene_view.cpp:34-51`): the
    /// prism sweep.
    pub fn collide_prism(
        &self,
        start: &WorldTriangle,
        end: &Vector3,
        cc: &mut dyn CollisionCallback,
    ) -> f32 {
        let shape = SweepShape::Prism(*start);
        let sp = SweepParams::new(&shape, *end);
        let mut cs = CollisionState::new(cc);

        for provider in &self.providers {
            if provider.collide_prism(start, end, &sp, &mut cs) {
                return cs.dist;
            }
        }

        cs.dist
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Stops the sweep when asked, counting its visits.
    struct RecordingProvider {
        stops: bool,
        visits: Cell<u32>,
    }

    impl ICollisionSceneViewProvider for RecordingProvider {
        fn collide_ray(
            &self,
            _source: &Vector3,
            _extent: &Vector3,
            sp: &SweepParams,
            state: &mut CollisionState,
        ) -> bool {
            self.visits.set(self.visits.get() + 1);
            if self.stops {
                state.dist = sp.full_distance() * 0.5;
                return true;
            }
            false
        }

        fn collide_prism(
            &self,
            _source: &WorldTriangle,
            _extent: &Vector3,
            sp: &SweepParams,
            state: &mut CollisionState,
        ) -> bool {
            if self.stops {
                state.dist = sp.full_distance() * 0.25;
                return true;
            }
            false
        }
    }

    struct NopCallback;
    impl CollisionCallback for NopCallback {}

    #[test]
    fn the_view_walks_providers_and_stops_at_the_first_hit() {
        let mut view = CollisionSceneView::new();
        assert!(view.scene().is_none());
        view.set_scene(Some(3));
        assert_eq!(view.scene(), Some(3));

        let pass = Rc::new(RecordingProvider {
            stops: false,
            visits: Cell::new(0),
        });
        let stop = Rc::new(RecordingProvider {
            stops: true,
            visits: Cell::new(0),
        });
        view.add_provider(pass.clone());
        view.add_provider(stop.clone());
        assert_eq!(view.providers_len(), 2);

        // A stopping provider answers its half-way dist.
        let dist = view.collide_ray(
            &Vector3::ZERO,
            &Vector3::new(0.0, 0.0, 10.0),
            &mut NopCallback,
        );
        assert_eq!(dist, 5.0);
        assert_eq!(pass.visits.get(), 1, "the first provider ran first");
        assert_eq!(stop.visits.get(), 1, "the walk stopped at the second");

        // Removal by identity unwires exactly that face.
        let as_face: Rc<dyn ICollisionSceneViewProvider> = stop.clone();
        view.remove_provider(&as_face);
        assert_eq!(view.providers_len(), 1);
        let dist = view.collide_ray(
            &Vector3::ZERO,
            &Vector3::new(0.0, 0.0, 10.0),
            &mut NopCallback,
        );
        assert_eq!(dist, -1.0, "no provider stops: the initial dist answers");
    }

    #[test]
    fn the_prism_face_stops_and_reports_the_state_dist() {
        let mut view = CollisionSceneView::new();
        let provider = Rc::new(RecordingProvider {
            stops: true,
            visits: Cell::new(0),
        });
        view.add_provider(provider);

        let triangle = WorldTriangle::new(
            Vector3::new(-1.0, -1.0, 0.0),
            Vector3::new(1.0, -1.0, 0.0),
            Vector3::new(0.0, 1.0, 0.0),
            0,
        );
        // A stopping provider answers a quarter-way dist of the sweep
        // parameters the view assembled.
        let sp = SweepParams::new(&SweepShape::Prism(triangle), Vector3::new(0.0, 0.0, 4.0));
        let dist = view.collide_prism(&triangle, &Vector3::new(0.0, 0.0, 4.0), &mut NopCallback);
        assert_eq!(dist, sp.full_distance() * 0.25);
    }
}
